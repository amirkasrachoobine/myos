// ps: report processes, as sysinfod sees them (bus service "sysinfo").

#include <unistd.h>

#include <algorithm>
#include <cstring>
#include <iostream>
#include <set>

#include "common.hpp"
#include "procs.hpp"

using namespace myos::util;

namespace {

const char* kUsage =
    "usage: ps [-efl] [aux] [-o FIELDS] [-p PIDS] [-u USERS] [-C NAMES] [--sort [-]FIELD,...] [-w] [--no-headers]\n"
    "  (all processes are shown; there are no terminals to filter by)\n"
    "  -f       full format: USER PID PPID %CPU START TIME COMMAND\n"
    "  -l       long format: STAT UID PID PPID %CPU NI RSS VSZ TIME COMMAND\n"
    "  aux, u   USER PID %CPU %MEM VSZ RSS STAT START TIME COMMAND\n"
    "  -o       fields, comma-separated; FIELD=HEADER renames a column\n"
    "  --sort   e.g. --sort=-%cpu,pid  (- for descending)\n"
    "  -w       don't cut lines at the terminal width\n"
    "Fields: pid ppid uid user stat nlwp ni %cpu %mem rss vsz start etime time comm args\n"
    "Data comes from sysinfod over the bus (myos-sysinfo ps shows the same).\n";

struct Column {
  const Field* field;
  std::string header;
};

[[noreturn]] void bad(const std::string& msg) {
  std::cerr << "ps: " << msg << "\n" << kUsage;
  std::exit(2);
}

void add_fields(std::vector<Column>& cols, const std::string& spec) {
  // "pid,user" or "pid user" or "pid=ID"; a header with = takes the rest.
  std::string s = spec;
  size_t eq = s.find('=');
  std::string tail_header;
  bool has_header = eq != std::string::npos;
  if (has_header) {
    tail_header = s.substr(eq + 1);
    s = s.substr(0, eq);
  }
  for (char& c : s) {
    if (c == ' ') c = ',';
  }
  std::vector<std::string> keys = split(s, ',');
  for (size_t i = 0; i < keys.size(); ++i) {
    if (keys[i].empty()) continue;
    const Field* f = find_field(keys[i]);
    if (!f) bad("unknown field '" + keys[i] + "'");
    std::string header = (has_header && i + 1 == keys.size()) ? tail_header : f->header;
    cols.push_back({f, header});
  }
}

std::set<std::string> list_arg(const std::string& s) {
  std::set<std::string> out;
  std::string t = s;
  for (char& c : t) {
    if (c == ' ') c = ',';
  }
  for (auto& x : split(t, ',')) {
    if (!x.empty()) out.insert(x);
  }
  return out;
}

}  // namespace

int main(int argc, char** argv) {
  std::vector<Column> cols;
  std::string format;  // a named format, if no -o
  std::set<std::string> pids, users, names;
  std::vector<std::pair<const Field*, bool>> sort;  // field, descending
  bool wide = false, headers = true;

  auto need = [&](int& i, const std::string& opt) -> std::string {
    if (i + 1 >= argc) bad(opt + " needs an argument");
    return argv[++i];
  };
  auto parse_sort = [&](const std::string& spec) {
    for (auto& k : split(spec, ',')) {
      if (k.empty()) continue;
      bool desc = k[0] == '-';
      std::string key = (k[0] == '-' || k[0] == '+') ? k.substr(1) : k;
      const Field* f = find_field(key);
      if (!f) bad("unknown sort field '" + key + "'");
      sort.emplace_back(f, desc);
    }
  };

  for (int i = 1; i < argc; ++i) {
    std::string a = argv[i];
    if (a == "-aux" || a == "-axu") a = "aux";  // muscle memory from procps
    if (a == "--help" || a == "-h") {
      std::cout << kUsage;
      return 0;
    }
    if (a == "--version" || a == "-V") {
      std::cout << "ps (myos) " MYOS_UTILS_VERSION "\n";
      return 0;
    }
    if (a.rfind("--sort", 0) == 0) {
      parse_sort(a.size() > 6 && a[6] == '=' ? a.substr(7) : need(i, "--sort"));
    } else if (a == "--no-headers" || a == "--no-heading") {
      headers = false;
    } else if (a.rfind("--pid", 0) == 0) {
      auto s = list_arg(a.size() > 5 && a[5] == '=' ? a.substr(6) : need(i, "--pid"));
      pids.insert(s.begin(), s.end());
    } else if (a.rfind("--user", 0) == 0) {
      auto s = list_arg(a.size() > 6 && a[6] == '=' ? a.substr(7) : need(i, "--user"));
      users.insert(s.begin(), s.end());
    } else if (a.rfind("--format", 0) == 0) {
      add_fields(cols, a.size() > 8 && a[8] == '=' ? a.substr(9) : need(i, "--format"));
    } else if (a.size() > 1 && a[0] == '-') {
      for (size_t k = 1; k < a.size(); ++k) {
        char c = a[k];
        std::string rest = a.substr(k + 1);
        auto value = [&]() { return rest.empty() ? need(i, std::string("-") + c) : rest; };
        if (c == 'e' || c == 'A' || c == 'a' || c == 'x' || c == 'N' || c == 'T') {
          continue;
        } else if (c == 'f' || c == 'F') {
          format = "full";
        } else if (c == 'l') {
          format = "long";
        } else if (c == 'w') {
          wide = true;
        } else if (c == 'o' || c == 'O') {
          add_fields(cols, value());
          break;
        } else if (c == 'p' || c == 'q') {
          auto s = list_arg(value());
          pids.insert(s.begin(), s.end());
          break;
        } else if (c == 'u' || c == 'U') {
          auto s = list_arg(value());
          users.insert(s.begin(), s.end());
          break;
        } else if (c == 'C') {
          auto s = list_arg(value());
          names.insert(s.begin(), s.end());
          break;
        } else {
          bad(std::string("unknown option -") + c);
        }
      }
    } else if (a.find_first_not_of("auxwfhe") == std::string::npos) {  // BSD style: aux
      for (char c : a) {
        if (c == 'u') format = "user";
        if (c == 'w') wide = true;
        if (c == 'h') headers = false;
      }
    } else if (a.find_first_not_of("0123456789,") == std::string::npos) {
      auto s = list_arg(a);
      pids.insert(s.begin(), s.end());
    } else {
      bad("unexpected argument '" + a + "'");
    }
  }
  if (cols.empty()) {
    const char* spec = format == "full"   ? "user,pid,ppid,%cpu,start,time,args"
                       : format == "long" ? "stat,uid,pid,ppid,%cpu,ni,rss,vsz,time,args"
                       : format == "user" ? "user,pid,%cpu,%mem,vsz,rss,stat,start,time,args"
                                          : "pid,user,time,args";
    add_fields(cols, spec);
  }
  bool any_header = false;
  for (auto& c : cols) any_header = any_header || !c.header.empty();
  if (!any_header) headers = false;  // ps -o pid= -o comm=
  std::set<uid_t> uids;
  for (const std::string& u : users) {
    uid_t id;
    if (!lookup_user(u, id)) {
      std::cerr << "ps: user name does not exist: " << u << "\n";
      return 1;
    }
    uids.insert(id);
  }

  Snapshot snap;
  try {
    SysinfoClient client;
    snap.processes = client.processes();
    bool need_system = false;
    for (auto& c : cols) need_system = need_system || !std::strcmp(c.field->key, "start") || !std::strcmp(c.field->key, "etime");
    for (auto& s : sort) need_system = need_system || !std::strcmp(s.first->key, "etime");
    if (need_system) snap.system = client.system();
  } catch (const myos::bus::Error& e) {
    std::cerr << "ps: " << explain(e) << "\n";
    return 1;
  }

  std::vector<const si::Process*> procs;
  for (const si::Process& p : snap.processes.processes()) {
    if (!pids.empty() && !pids.count(std::to_string(p.pid()))) continue;
    if (!uids.empty() && !uids.count(p.uid())) continue;
    if (!names.empty() && !names.count(p.name())) continue;
    procs.push_back(&p);
  }
  if (sort.empty()) sort.emplace_back(find_field("pid"), false);
  std::stable_sort(procs.begin(), procs.end(), [&](const si::Process* a, const si::Process* b) {
    for (auto& [f, desc] : sort) {
      if (f->num) {
        double x = f->num(*a, snap), y = f->num(*b, snap);
        if (x != y) return desc ? x > y : x < y;
      } else {
        std::string x = f->get(*a, snap), y = f->get(*b, snap);
        if (x != y) return desc ? x > y : x < y;
      }
    }
    return false;
  });

  Table t;
  for (auto& c : cols) t.right.push_back(c.field->right);
  if (headers) {
    std::vector<std::string> h;
    for (auto& c : cols) h.push_back(c.header);
    t.rows.push_back(h);
  }
  for (const si::Process* p : procs) {
    std::vector<std::string> r;
    for (auto& c : cols) r.push_back(c.field->get(*p, snap));
    t.rows.push_back(r);
  }
  int width = wide ? 0 : term_width(1);
  std::string out;
  for (std::string& line : t.render()) {
    if (width > 0 && utf8_width(line) > width) {
      size_t cut = 0;
      int w = 0;
      while (cut < line.size() && w < width) {
        ++cut;
        while (cut < line.size() && (static_cast<unsigned char>(line[cut]) & 0xC0) == 0x80) ++cut;
        ++w;
      }
      line.resize(cut);
    }
    out += line + "\n";
  }
  std::cout << out << std::flush;
  bool filtered = !pids.empty() || !uids.empty() || !names.empty();
  return filtered && procs.empty() ? 1 : 0;
}
