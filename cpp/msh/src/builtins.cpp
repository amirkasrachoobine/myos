// Builtin commands. Each gets the expanded argv and returns an exit status;
// output goes to fds 1 and 2 directly, so redirections apply.

#include <errno.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include <algorithm>
#include <cstring>

#include "shell.hpp"

namespace msh {

namespace {

void out(const std::string& s) { write_all(1, s); }

int usage_error(Shell& sh, const std::string& name, const std::string& msg) {
  sh.err(name + ": " + msg);
  return 2;
}

std::string quote(const std::string& v) {
  std::string q = "'";
  for (char c : v) {
    if (c == '\'') {
      q += "'\\''";
    } else {
      q += c;
    }
  }
  return q + "'";
}

bool parse_int(const std::string& s, long& v) {
  if (s.empty()) return false;
  char* end = nullptr;
  errno = 0;
  v = std::strtol(s.c_str(), &end, 10);
  return errno == 0 && end && *end == '\0';
}

// Lexically normalize an absolute path: drop ".", resolve "..", squeeze "//".
std::string normalize(const std::string& path) {
  std::vector<std::string> parts;
  size_t i = 0;
  while (i <= path.size()) {
    size_t j = path.find('/', i);
    if (j == std::string::npos) j = path.size();
    std::string p = path.substr(i, j - i);
    if (p == "..") {
      if (!parts.empty()) parts.pop_back();
    } else if (!p.empty() && p != ".") {
      parts.push_back(p);
    }
    i = j + 1;
  }
  return "/" + join(parts, "/");
}

int b_true(Shell&, std::vector<std::string>&) { return 0; }
int b_false(Shell&, std::vector<std::string>&) { return 1; }

int b_cd(Shell& sh, std::vector<std::string>& a) {
  std::string target;
  bool print = false;
  if (a.size() > 2) return usage_error(sh, "cd", "too many arguments");
  if (a.size() == 1) {
    target = sh.get_or("HOME");
    if (target.empty()) return usage_error(sh, "cd", "HOME not set") - 1;
  } else if (a[1] == "-") {
    target = sh.get_or("OLDPWD");
    if (target.empty()) return usage_error(sh, "cd", "OLDPWD not set") - 1;
    print = true;
  } else {
    target = a[1];
  }
  std::string old = sh.get_or("PWD");
  if (old.empty()) {
    char buf[4096];
    if (getcwd(buf, sizeof buf)) old = buf;
  }
  std::string logical = normalize(target[0] == '/' ? target : old + "/" + target);
  std::string pwd;
  if (chdir(logical.c_str()) == 0) {
    pwd = logical;
  } else if (chdir(target.c_str()) == 0) {
    char buf[4096];
    pwd = getcwd(buf, sizeof buf) ? buf : logical;
  } else {
    sh.err("cd: " + target + ": " + std::strerror(errno));
    return 1;
  }
  sh.set("OLDPWD", old, true);
  sh.set("PWD", pwd, true);
  if (print) out(pwd + "\n");
  return 0;
}

int b_pwd(Shell& sh, std::vector<std::string>& a) {
  bool physical = a.size() > 1 && a[1] == "-P";
  std::string pwd = sh.get_or("PWD");
  struct stat x, y;
  if (physical || pwd.empty() || stat(pwd.c_str(), &x) != 0 || stat(".", &y) != 0 || x.st_ino != y.st_ino ||
      x.st_dev != y.st_dev) {
    char buf[4096];
    if (!getcwd(buf, sizeof buf)) {
      sh.err(std::string("pwd: ") + std::strerror(errno));
      return 1;
    }
    pwd = buf;
  }
  out(pwd + "\n");
  return 0;
}

int b_exit(Shell& sh, std::vector<std::string>& a) {
  long code = sh.last_status;
  if (a.size() > 1 && !parse_int(a[1], code)) {
    sh.err("exit: " + a[1] + ": numeric argument required");
    code = 2;
  }
  if (sh.interactive && !sh.warned_stopped) {
    for (const Job& j : sh.jobs) {
      if (j.stopped()) {
        sh.err("there are stopped jobs; `exit' again to leave anyway");
        sh.warned_stopped = true;
        return 1;
      }
    }
  }
  if (sh.interactive && !sh.in_child) write_all(2, "exit\n");
  sh.exiting = true;
  sh.exit_code = static_cast<int>(code & 0xff);
  return sh.exit_code;
}

int b_echo(Shell&, std::vector<std::string>& a) {
  bool newline = true, escapes = false;
  size_t i = 1;
  for (; i < a.size(); ++i) {
    const std::string& f = a[i];
    if (f.size() < 2 || f[0] != '-' || f.find_first_not_of("neE", 1) != std::string::npos) break;
    for (char c : f.substr(1)) {
      if (c == 'n') newline = false;
      if (c == 'e') escapes = true;
      if (c == 'E') escapes = false;
    }
  }
  std::string s;
  for (size_t k = i; k < a.size(); ++k) {
    if (k > i) s += ' ';
    if (!escapes) {
      s += a[k];
      continue;
    }
    const std::string& w = a[k];
    for (size_t j = 0; j < w.size(); ++j) {
      if (w[j] != '\\' || j + 1 >= w.size()) {
        s += w[j];
        continue;
      }
      char c = w[++j];
      switch (c) {
        case 'n': s += '\n'; break;
        case 't': s += '\t'; break;
        case 'r': s += '\r'; break;
        case 'a': s += '\a'; break;
        case 'b': s += '\b'; break;
        case 'f': s += '\f'; break;
        case 'v': s += '\v'; break;
        case 'e': s += '\033'; break;
        case '\\': s += '\\'; break;
        case 'c':
          out(s);
          return 0;
        case '0': {
          int v = 0, n = 0;
          while (n < 3 && j + 1 < w.size() && w[j + 1] >= '0' && w[j + 1] <= '7') v = v * 8 + (w[++j] - '0'), ++n;
          s += static_cast<char>(v);
          break;
        }
        case 'x': {
          int v = 0, n = 0;
          while (n < 2 && j + 1 < w.size() && std::isxdigit(static_cast<unsigned char>(w[j + 1]))) {
            char h = w[++j];
            v = v * 16 + (std::isdigit(static_cast<unsigned char>(h)) ? h - '0' : (std::tolower(h) - 'a' + 10));
            ++n;
          }
          if (n) {
            s += static_cast<char>(v);
          } else {
            s += "\\x";
          }
          break;
        }
        default:
          s += '\\';
          s += c;
      }
    }
  }
  if (newline) s += '\n';
  out(s);
  return 0;
}

int b_export(Shell& sh, std::vector<std::string>& a) {
  if (a.size() == 1 || (a.size() == 2 && a[1] == "-p")) {
    for (auto& [k, v] : sh.vars) {
      if (v.exported) out("export " + k + "=" + quote(v.value) + "\n");
    }
    return 0;
  }
  int st = 0;
  for (size_t i = 1; i < a.size(); ++i) {
    size_t eq = a[i].find('=');
    std::string name = a[i].substr(0, eq);
    if (!is_name(name)) {
      sh.err("export: `" + a[i] + "': not a valid identifier");
      st = 1;
      continue;
    }
    if (eq != std::string::npos) {
      sh.set(name, a[i].substr(eq + 1), true);
    } else {
      sh.vars[name].exported = true;
    }
  }
  return st;
}

int b_unset(Shell& sh, std::vector<std::string>& a) {
  bool funcs = false;
  for (size_t i = 1; i < a.size(); ++i) {
    if (a[i] == "-f") {
      funcs = true;
    } else if (a[i] == "-v") {
      funcs = false;
    } else if (funcs) {
      sh.functions.erase(a[i]);
    } else {
      sh.unset(a[i]);
    }
  }
  return 0;
}

int b_alias(Shell& sh, std::vector<std::string>& a) {
  if (a.size() == 1) {
    for (auto& [k, v] : sh.aliases) out("alias " + k + "=" + quote(v) + "\n");
    return 0;
  }
  int st = 0;
  for (size_t i = 1; i < a.size(); ++i) {
    size_t eq = a[i].find('=');
    if (eq == std::string::npos) {
      auto it = sh.aliases.find(a[i]);
      if (it == sh.aliases.end()) {
        sh.err("alias: " + a[i] + ": not found");
        st = 1;
      } else {
        out("alias " + it->first + "=" + quote(it->second) + "\n");
      }
    } else {
      sh.aliases[a[i].substr(0, eq)] = a[i].substr(eq + 1);
    }
  }
  return st;
}

int b_unalias(Shell& sh, std::vector<std::string>& a) {
  int st = 0;
  for (size_t i = 1; i < a.size(); ++i) {
    if (a[i] == "-a") {
      sh.aliases.clear();
    } else if (!sh.aliases.erase(a[i])) {
      sh.err("unalias: " + a[i] + ": not found");
      st = 1;
    }
  }
  return st;
}

int b_jobs(Shell& sh, std::vector<std::string>& a) {
  bool pids = false, long_fmt = false;
  for (size_t i = 1; i < a.size(); ++i) {
    if (a[i] == "-p") pids = true;
    else if (a[i] == "-l") long_fmt = true;
    else return usage_error(sh, "jobs", "usage: jobs [-lp]");
  }
  sh.reap(false);
  std::vector<int> done;
  for (Job& j : sh.jobs) {
    if (pids) {
      out(std::to_string(j.procs[0].pid) + "\n");
    } else {
      out(sh.job_line(j, long_fmt) + "\n");
    }
    j.notify = false;
    if (j.all_done()) done.push_back(j.id);
  }
  for (int id : done) sh.remove_job(id);
  return 0;
}

int fg_bg(Shell& sh, std::vector<std::string>& a, bool fg) {
  const char* name = fg ? "fg" : "bg";
  if (!sh.job_control) {
    sh.err(std::string(name) + ": no job control");
    return 1;
  }
  std::string e;
  Job* j = sh.find_job(a.size() > 1 ? a[1] : "", e);
  if (!j) {
    sh.err(std::string(name) + ": " + e);
    return 1;
  }
  if (!fg && !j->stopped()) {
    sh.err("bg: job " + std::to_string(j->id) + " already in background");
    return 0;
  }
  return sh.continue_job(*j, fg);
}
int b_fg(Shell& sh, std::vector<std::string>& a) { return fg_bg(sh, a, true); }
int b_bg(Shell& sh, std::vector<std::string>& a) { return fg_bg(sh, a, false); }

int b_wait(Shell& sh, std::vector<std::string>& a) {
  auto wait_job = [&](int id) -> int {
    for (;;) {
      Job* j = sh.job_by_id(id);
      if (!j) return 127;
      if (j->all_done() || j->stopped()) {
        int st = j->status();
        if (j->all_done()) sh.remove_job(id);
        return st;
      }
      if (g_sigint && sh.interactive) return 130;
      sh.reap(true);
    }
  };
  int st = 0;
  if (a.size() == 1) {
    while (!sh.jobs.empty()) {
      int id = sh.jobs.front().id;
      st = wait_job(id);
      if (st == 130 && g_sigint) return st;
      if (sh.job_by_id(id)) break;  // stopped
    }
    return 0;
  }
  for (size_t i = 1; i < a.size(); ++i) {
    int id = 0;
    if (a[i][0] == '%') {
      std::string e;
      Job* j = sh.find_job(a[i], e);
      if (!j) {
        sh.err("wait: " + e);
        st = 127;
        continue;
      }
      id = j->id;
    } else {
      long pid;
      if (!parse_int(a[i], pid)) {
        sh.err("wait: `" + a[i] + "': not a pid or valid job spec");
        st = 2;
        continue;
      }
      for (Job& j : sh.jobs) {
        for (Proc& p : j.procs) {
          if (p.pid == pid) id = j.id;
        }
      }
      if (!id) {
        sh.err("wait: pid " + a[i] + " is not a child of this shell");
        st = 127;
        continue;
      }
    }
    st = wait_job(id);
  }
  return st;
}

int b_kill(Shell& sh, std::vector<std::string>& a) {
  int sig = SIGTERM;
  size_t i = 1;
  if (i < a.size() && a[i] == "-l") {
    if (a.size() > 2) {
      long n;
      if (parse_int(a[2], n)) {
        if (n > 128) n -= 128;
        std::string name = signal_name(static_cast<int>(n));
        if (name.empty()) return usage_error(sh, "kill", a[2] + ": invalid signal specification") - 1;
        out(name + "\n");
      } else {
        int s = signal_number(a[2]);
        if (s < 0) return usage_error(sh, "kill", a[2] + ": invalid signal specification") - 1;
        out(std::to_string(s) + "\n");
      }
      return 0;
    }
    std::string line;
    for (int s = 1; s < 32; ++s) {
      std::string name = signal_name(s);
      if (!name.empty()) line += std::to_string(s) + ") SIG" + name + (s % 6 == 0 ? "\n" : "\t");
    }
    out(line + "\n");
    return 0;
  }
  if (i < a.size() && a[i] == "-s") {
    if (i + 1 >= a.size()) return usage_error(sh, "kill", "-s: option requires an argument");
    sig = signal_number(a[i + 1]);
    if (sig < 0) return usage_error(sh, "kill", a[i + 1] + ": invalid signal specification") - 1;
    i += 2;
  } else if (i < a.size() && a[i].size() > 1 && a[i][0] == '-' && a[i] != "--") {
    sig = signal_number(a[i].substr(1));
    if (sig < 0) return usage_error(sh, "kill", a[i] + ": invalid signal specification") - 1;
    ++i;
  }
  if (i < a.size() && a[i] == "--") ++i;
  if (i >= a.size()) return usage_error(sh, "kill", "usage: kill [-s sigspec | -sigspec] pid | %job ... or kill -l");
  int st = 0;
  for (; i < a.size(); ++i) {
    pid_t target;
    if (a[i][0] == '%') {
      std::string e;
      Job* j = sh.find_job(a[i], e);
      if (!j) {
        sh.err("kill: " + e);
        st = 1;
        continue;
      }
      target = sh.job_control ? -j->pgid : j->procs.back().pid;
      if (sh.job_control && (sig == SIGTERM || sig == SIGHUP) && j->stopped()) kill(target, SIGCONT);
    } else {
      long p;
      if (!parse_int(a[i], p)) {
        sh.err("kill: " + a[i] + ": arguments must be process or job IDs");
        st = 1;
        continue;
      }
      target = static_cast<pid_t>(p);
    }
    if (kill(target, sig) < 0) {
      sh.err("kill: (" + a[i] + ") - " + std::strerror(errno));
      st = 1;
    }
  }
  return st;
}

int b_source(Shell& sh, std::vector<std::string>& a) {
  if (a.size() < 2) return usage_error(sh, a[0], "filename argument required");
  std::string path = a[1];
  if (path.find('/') == std::string::npos && access(path.c_str(), R_OK) != 0) {
    std::string found = sh.find_command(path);
    if (!found.empty()) path = found;
  }
  std::vector<std::string> saved = sh.positional;
  bool args = a.size() > 2;
  if (args) sh.positional.assign(a.begin() + 2, a.end());
  ++sh.func_depth;  // so `return` works in a sourced file
  int st = sh.run_file(path);
  --sh.func_depth;
  if (sh.returning) {
    sh.returning = false;
    st = sh.last_status;
  }
  if (args) sh.positional = saved;
  return st;
}

int b_history(Shell& sh, std::vector<std::string>& a) {
  if (!sh.history_list) return 0;
  if (a.size() > 1 && a[1] == "-c") {
    if (sh.history_clear) sh.history_clear();
    return 0;
  }
  std::vector<std::string> h = sh.history_list();
  size_t start = 0;
  if (a.size() > 1) {
    long n;
    if (!parse_int(a[1], n) || n < 0) return usage_error(sh, "history", a[1] + ": numeric argument required");
    if (static_cast<size_t>(n) < h.size()) start = h.size() - n;
  }
  std::string s;
  for (size_t i = start; i < h.size(); ++i) {
    char num[32];
    std::snprintf(num, sizeof num, "%5zu  ", i + 1);
    s += num + h[i] + "\n";
  }
  out(s);
  return 0;
}

int b_type(Shell& sh, std::vector<std::string>& a) {
  int st = 0;
  for (size_t i = 1; i < a.size(); ++i) {
    const std::string& n = a[i];
    if (sh.aliases.count(n)) {
      out(n + " is aliased to `" + sh.aliases[n] + "'\n");
    } else if (is_keyword(n)) {
      out(n + " is a shell keyword\n");
    } else if (sh.functions.count(n)) {
      out(n + " is a function\n");
    } else if (builtins().count(n) || n == "exec") {
      out(n + " is a shell builtin\n");
    } else {
      std::string p = sh.find_command(n);
      if (p.empty() || access(p.c_str(), X_OK) != 0) {
        sh.err("type: " + n + ": not found");
        st = 1;
      } else {
        out(n + " is " + p + "\n");
      }
    }
  }
  return st;
}

int b_read(Shell& sh, std::vector<std::string>& a) {
  bool raw = false;
  std::string prompt;
  size_t i = 1;
  for (; i < a.size() && a[i][0] == '-' && a[i].size() > 1; ++i) {
    if (a[i] == "-r") {
      raw = true;
    } else if (a[i] == "-p" && i + 1 < a.size()) {
      prompt = a[++i];
    } else if (a[i] == "--") {
      ++i;
      break;
    } else {
      return usage_error(sh, "read", "usage: read [-r] [-p prompt] [name ...]");
    }
  }
  std::vector<std::string> names(a.begin() + i, a.end());
  if (names.empty()) names.push_back("REPLY");
  if (!prompt.empty() && isatty(0)) write_all(2, prompt);
  std::string line;
  std::vector<bool> escaped;  // characters that came from a backslash escape
  bool eof = true;
  for (;;) {
    char c;
    ssize_t n = read(0, &c, 1);
    if (n < 0 && errno == EINTR) {
      if (g_sigint) return 130;
      continue;
    }
    if (n <= 0) break;
    if (c == '\n') {
      eof = false;
      break;
    }
    if (c == '\\' && !raw) {
      char d;
      if (read(0, &d, 1) != 1) break;
      if (d == '\n') continue;
      line += d;
      escaped.push_back(true);
      continue;
    }
    line += c;
    escaped.push_back(false);
  }
  std::string ifs = sh.get("IFS").value_or(" \t\n");
  auto is_ifs = [&](size_t k) { return !escaped[k] && ifs.find(line[k]) != std::string::npos; };
  size_t k = 0;
  for (size_t v = 0; v < names.size(); ++v) {
    while (k < line.size() && is_ifs(k) && std::isspace(static_cast<unsigned char>(line[k]))) ++k;
    std::string val;
    if (v + 1 == names.size()) {
      size_t end = line.size();
      while (end > k && is_ifs(end - 1) && std::isspace(static_cast<unsigned char>(line[end - 1]))) --end;
      val = line.substr(k, end - k);
      k = line.size();
    } else {
      size_t b = k;
      while (k < line.size() && !is_ifs(k)) ++k;
      val = line.substr(b, k - b);
      if (k < line.size()) ++k;
    }
    if (!is_name(names[v])) {
      sh.err("read: `" + names[v] + "': not a valid identifier");
      return 1;
    }
    sh.set(names[v], val);
  }
  return eof ? 1 : 0;  // a last line without a newline still sets the variables
}

int b_shift(Shell& sh, std::vector<std::string>& a) {
  long n = 1;
  if (a.size() > 1 && (!parse_int(a[1], n) || n < 0)) return usage_error(sh, "shift", a[1] + ": numeric argument required");
  if (static_cast<size_t>(n) > sh.positional.size()) return 1;
  sh.positional.erase(sh.positional.begin(), sh.positional.begin() + n);
  return 0;
}

int b_set(Shell& sh, std::vector<std::string>& a) {
  if (a.size() == 1) {
    for (auto& [k, v] : sh.vars) out(k + "=" + quote(v.value) + "\n");
    return 0;
  }
  size_t i = 1;
  for (; i < a.size(); ++i) {
    const std::string& f = a[i];
    if (f == "--") {
      ++i;
      sh.positional.assign(a.begin() + i, a.end());
      return 0;
    }
    if (f == "-o" || f == "+o") {
      if (i + 1 >= a.size()) {
        out(std::string("errexit\t") + (sh.opt_errexit ? "on" : "off") + "\nxtrace\t" + (sh.opt_xtrace ? "on" : "off") + "\n");
        return 0;
      }
      bool on = f[0] == '-';
      const std::string& o = a[++i];
      if (o == "errexit") sh.opt_errexit = on;
      else if (o == "xtrace") sh.opt_xtrace = on;
      else return usage_error(sh, "set", o + ": invalid option name");
      continue;
    }
    if (f.size() > 1 && (f[0] == '-' || f[0] == '+')) {
      bool on = f[0] == '-';
      for (char c : f.substr(1)) {
        if (c == 'e') sh.opt_errexit = on;
        else if (c == 'x') sh.opt_xtrace = on;
        else return usage_error(sh, "set", std::string(1, f[0]) + c + ": invalid option");
      }
      continue;
    }
    break;
  }
  if (i < a.size()) sh.positional.assign(a.begin() + i, a.end());
  return 0;
}

int b_return(Shell& sh, std::vector<std::string>& a) {
  if (sh.func_depth == 0) {
    sh.err("return: can only `return' from a function or sourced script");
    return 1;
  }
  long n = sh.last_status;
  if (a.size() > 1 && !parse_int(a[1], n)) {
    sh.err("return: " + a[1] + ": numeric argument required");
    n = 2;
  }
  sh.returning = true;
  sh.last_status = static_cast<int>(n & 0xff);
  return sh.last_status;
}

int loop_ctl(Shell& sh, std::vector<std::string>& a, bool brk) {
  long n = 1;
  if (a.size() > 1 && (!parse_int(a[1], n) || n < 1)) {
    sh.err(a[0] + ": " + a[1] + ": loop count out of range");
    return 1;
  }
  if (sh.loop_depth == 0) {
    sh.err(a[0] + ": only meaningful in a `for', `while', or `until' loop");
    return 0;
  }
  int levels = static_cast<int>(std::min<long>(n, sh.loop_depth));
  if (brk) {
    sh.break_levels = levels;
  } else {
    sh.continue_levels = levels;
  }
  return 0;
}
int b_break(Shell& sh, std::vector<std::string>& a) { return loop_ctl(sh, a, true); }
int b_continue(Shell& sh, std::vector<std::string>& a) { return loop_ctl(sh, a, false); }

int b_local(Shell& sh, std::vector<std::string>& a) {
  if (sh.locals.empty()) {
    sh.err("local: can only be used in a function");
    return 1;
  }
  for (size_t i = 1; i < a.size(); ++i) {
    size_t eq = a[i].find('=');
    std::string name = a[i].substr(0, eq);
    if (!is_name(name)) {
      sh.err("local: `" + a[i] + "': not a valid identifier");
      return 1;
    }
    auto& frame = sh.locals.back();
    bool recorded = std::any_of(frame.begin(), frame.end(), [&](auto& p) { return p.first == name; });
    if (!recorded) {
      auto it = sh.vars.find(name);
      frame.emplace_back(name, it == sh.vars.end() ? std::nullopt : std::optional<Var>(it->second));
    }
    sh.vars[name] = Var{eq == std::string::npos ? "" : a[i].substr(eq + 1), false};
  }
  return 0;
}

int b_eval(Shell& sh, std::vector<std::string>& a) {
  std::vector<std::string> rest(a.begin() + 1, a.end());
  return sh.run_string(join(rest, " "));
}

int b_umask(Shell& sh, std::vector<std::string>& a) {
  if (a.size() == 1) {
    mode_t m = umask(0);
    umask(m);
    char buf[8];
    std::snprintf(buf, sizeof buf, "%04o\n", static_cast<unsigned>(m));
    out(buf);
    return 0;
  }
  char* end = nullptr;
  long m = std::strtol(a[1].c_str(), &end, 8);
  if (!end || *end || m < 0 || m > 0777) return usage_error(sh, "umask", a[1] + ": octal number out of range") - 1;
  umask(static_cast<mode_t>(m));
  return 0;
}

int b_resize(Shell& sh, std::vector<std::string>&) {
  if (sh.resize) sh.resize();
  return 0;
}

int b_help(Shell&, std::vector<std::string>&) {
  std::string s =
      "msh, the myos shell. Builtins:\n"
      "  cd [dir|-]  pwd [-P]  echo [-neE]  read [-r] [-p prompt] [name...]  exit [n]\n"
      "  export [name[=value]...]  unset [-f] name...  local name[=value]...  set [-ex] [-- args]\n"
      "  shift [n]  alias [name=value...]  unalias [-a] name...  type name...  eval args\n"
      "  source file  . file  history [-c] [n]  umask [mode]  true  false  :\n"
      "  jobs [-lp]  fg [%job]  bg [%job]  wait [%job|pid...]  kill [-sig] pid|%job...  kill -l\n"
      "  break [n]  continue [n]  return [n]  exec [cmd...]  resize  help\n"
      "Syntax: pipes |, lists ; && || &, redirections < > >> 2> 2>&1 &> <>, quotes ' \" \\,\n"
      "  $var ${var:-default} ${#var} ${var%suffix} $(cmd) `cmd` $((1 + 2)) ~ * ? [...],\n"
      "  if/elif/else/fi, while/until/do/done, for x in ...; do ...; done, { ...; }, ( ... ),\n"
      "  name() { ...; }, # comments.\n"
      "Line editing: arrows, Home/End, Ctrl-A/E/K/U/W/Y/L, Ctrl-R search, Tab completion,\n"
      "  !! !n !-n !prefix history expansion. Ctrl-Z stops a job; fg/bg resume it.\n"
      "Startup: /etc/mshrc, then ~/.mshrc (interactive shells). History: ~/.msh_history.\n";
  out(s);
  return 0;
}

}  // namespace

const std::map<std::string, Builtin>& builtins() {
  static const std::map<std::string, Builtin> m = {
      {":", b_true},         {"true", b_true},     {"false", b_false},     {"cd", b_cd},       {"pwd", b_pwd},
      {"exit", b_exit},      {"logout", b_exit},   {"echo", b_echo},       {"export", b_export},
      {"unset", b_unset},    {"alias", b_alias},   {"unalias", b_unalias}, {"jobs", b_jobs},   {"fg", b_fg},
      {"bg", b_bg},          {"wait", b_wait},     {"kill", b_kill},       {"source", b_source},
      {".", b_source},       {"history", b_history}, {"type", b_type},     {"read", b_read},   {"shift", b_shift},
      {"set", b_set},        {"return", b_return}, {"break", b_break},     {"continue", b_continue},
      {"local", b_local},    {"eval", b_eval},     {"umask", b_umask},     {"resize", b_resize},
      {"help", b_help},
  };
  return m;
}

}  // namespace msh
