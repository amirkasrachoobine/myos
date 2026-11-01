#include "procs.hpp"

#include <time.h>

#include <cstdio>

namespace myos::util {

template <class Reply, class Request>
Reply SysinfoClient::call(const char* method, const Request& req) {
  for (int attempt = 0;; ++attempt) {
    try {
      if (!bus_) bus_.emplace(myos::bus::Client::connect());
      return bus_->call<Reply>("sysinfo", method, req);
    } catch (const myos::bus::Error& e) {
      bus_.reset();
      // A stale connection (busd restarted since): one fresh try.
      if (attempt == 0 && e.kind() == myos::bus::Error::Kind::Io) continue;
      throw;
    }
  }
}

si::ProcessesReply SysinfoClient::processes() { return call<si::ProcessesReply>("Processes", si::ProcessesRequest{}); }
si::SystemReply SysinfoClient::system() { return call<si::SystemReply>("System", si::SystemRequest{}); }

Snapshot SysinfoClient::snapshot() {
  Snapshot s;
  s.system = system();
  s.cpu = call<si::CpuReply>("Cpu", si::CpuRequest{});
  s.memory = call<si::MemoryReply>("Memory", si::MemoryRequest{});
  s.processes = processes();
  return s;
}

std::string explain(const myos::bus::Error& e) {
  std::string why = e.what();
  if (e.kind() == myos::bus::Error::Kind::Io) {
    return "can't reach the bus: " + why + " (is busd running? try `initctl status busd`)";
  }
  if (e.kind() == myos::bus::Error::Kind::Status && e.code() == myos::bus::v1::Status::UNKNOWN_SERVICE) {
    return "sysinfod isn't on the bus (try `initctl status sysinfo`)";
  }
  return "sysinfo: " + why;
}

std::string clock_time(unsigned long long s, bool always_hours) {
  char buf[48];
  unsigned long long d = s / 86400, h = (s / 3600) % 24, m = (s / 60) % 60, sec = s % 60;
  if (d) {
    std::snprintf(buf, sizeof buf, "%llu-%02llu:%02llu:%02llu", d, h, m, sec);
  } else if (h || always_hours) {
    std::snprintf(buf, sizeof buf, "%02llu:%02llu:%02llu", h, m, sec);
  } else {
    std::snprintf(buf, sizeof buf, "%02llu:%02llu", m, sec);
  }
  return buf;
}

// Control characters (a newline in an argument, say) would break the
// one-line-per-process layout; show them as '?', like procps.
static std::string printable(const std::string& s) {
  std::string r = s;
  for (char& c : r) {
    if (static_cast<unsigned char>(c) < 32 || c == 127) c = '?';
  }
  return r;
}

std::string command_text(const si::Process& p, bool full) {
  if (!full) return printable(p.name());
  if (p.cmdline().empty()) return "[" + printable(p.name()) + "]";
  return printable(p.cmdline());
}

namespace {

std::string fixed1(double v) {
  char buf[32];
  std::snprintf(buf, sizeof buf, "%.1f", v);
  return buf;
}

double mem_pct(const si::Process& p, const Snapshot& s) {
  uint64_t total = s.processes.memory_total_kb();
  return total ? 100.0 * static_cast<double>(p.rss_kb()) / static_cast<double>(total) : 0.0;
}

std::string start_text(const si::Process& p, const Snapshot& s) {
  time_t t = static_cast<time_t>(s.system.boot_time() + static_cast<long long>(p.start_seconds()));
  time_t now = time(nullptr);
  struct tm tm {}, today {};
  localtime_r(&t, &tm);
  localtime_r(&now, &today);
  char buf[32];
  if (tm.tm_yday == today.tm_yday && tm.tm_year == today.tm_year) {
    strftime(buf, sizeof buf, "%H:%M", &tm);
  } else if (tm.tm_year == today.tm_year) {
    strftime(buf, sizeof buf, "%b%d", &tm);
  } else {
    strftime(buf, sizeof buf, "%Y", &tm);
  }
  return buf;
}

double elapsed(const si::Process& p, const Snapshot& s) {
  double e = s.system.uptime_seconds() - p.start_seconds();
  return e < 0 ? 0 : e;
}

}  // namespace

const std::vector<Field>& fields() {
  using P = const si::Process&;
  using S = const Snapshot&;
  static const std::vector<Field> f = {
      {"pid", "PID", true, [](P p, S) { return std::to_string(p.pid()); }, [](P p, S) { return double(p.pid()); }},
      {"ppid", "PPID", true, [](P p, S) { return std::to_string(p.ppid()); }, [](P p, S) { return double(p.ppid()); }},
      {"uid", "UID", true, [](P p, S) { return std::to_string(p.uid()); }, [](P p, S) { return double(p.uid()); }},
      {"user", "USER", false, [](P p, S) { return p.user(); }, nullptr},
      {"stat", "STAT", false, [](P p, S) { return p.state(); }, nullptr},
      {"nlwp", "NLWP", true, [](P p, S) { return std::to_string(p.threads()); }, [](P p, S) { return double(p.threads()); }},
      {"ni", "NI", true, [](P p, S) { return std::to_string(p.nice()); }, [](P p, S) { return double(p.nice()); }},
      {"%cpu", "%CPU", true, [](P p, S) { return fixed1(p.cpu_percent()); }, [](P p, S) { return p.cpu_percent(); }},
      {"%mem", "%MEM", true, [](P p, S s) { return fixed1(mem_pct(p, s)); }, [](P p, S s) { return mem_pct(p, s); }},
      {"rss", "RSS", true, [](P p, S) { return std::to_string(p.rss_kb()); }, [](P p, S) { return double(p.rss_kb()); }},
      {"vsz", "VSZ", true, [](P p, S) { return std::to_string(p.vsize_kb()); }, [](P p, S) { return double(p.vsize_kb()); }},
      {"start", "START", false, [](P p, S s) { return start_text(p, s); }, [](P p, S) { return p.start_seconds(); }},
      {"etime", "ELAPSED", true,
       [](P p, S s) { return clock_time(static_cast<unsigned long long>(elapsed(p, s)), false); },
       [](P p, S s) { return elapsed(p, s); }},
      {"time", "TIME", true, [](P p, S) { return clock_time(p.cpu_time_ms() / 1000, true); },
       [](P p, S) { return double(p.cpu_time_ms()); }},
      {"comm", "COMMAND", false, [](P p, S) { return command_text(p, false); }, nullptr},
      {"args", "COMMAND", false, [](P p, S) { return command_text(p, true); }, nullptr},
  };
  return f;
}

const Field* find_field(const std::string& key) {
  static const std::vector<std::pair<const char*, const char*>> aliases = {
      {"pcpu", "%cpu"}, {"cpu", "%cpu"},   {"pmem", "%mem"},   {"mem", "%mem"},  {"cmd", "args"},
      {"command", "args"}, {"ucmd", "comm"}, {"name", "comm"},  {"vsize", "vsz"}, {"s", "stat"},
      {"state", "stat"}, {"nice", "ni"},    {"thcount", "nlwp"}, {"threads", "nlwp"}, {"cputime", "time"},
      {"lstart", "start"}, {"stime", "start"}, {"rssize", "rss"}, {"rsz", "rss"}, {"euser", "user"},
      {"uname", "user"},
  };
  std::string k = key;
  for (auto& a : aliases) {
    if (k == a.first) k = a.second;
  }
  for (const Field& f : fields()) {
    if (k == f.key) return &f;
  }
  return nullptr;
}

}  // namespace myos::util
