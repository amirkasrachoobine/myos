// top: a live view of the system and its busiest processes, from sysinfod
// (bus service "sysinfo"), which samples CPU use once a second.

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#include <algorithm>
#include <cmath>
#include <cstring>
#include <iostream>
#include <set>

#include "common.hpp"
#include "procs.hpp"

using namespace myos::util;

namespace {

const char* kUsage =
    "usage: top [-b] [-n COUNT] [-d SECONDS] [-o FIELD] [-p PIDS] [-u USER]\n"
    "  -b   batch mode: print plain frames (also used when output isn't a terminal)\n"
    "  -n   stop after COUNT frames     -d  seconds between frames (default 2)\n"
    "  -o   sort by: cpu mem pid time rss name (default cpu)\n"
    "  -p   only these pids             -u  only this user's processes\n"
    "Keys: q quit, P/M/N/T sort by cpu/memory/pid/time, R reverse, c full command,\n"
    "      1 per-CPU lines, k kill, d delay, space refresh, h help\n";

const char* kHelp =
    "Keys:  q or Ctrl-C  quit\n"
    "       P M N T      sort by %CPU, %MEM, PID, TIME+\n"
    "       R            reverse the sort\n"
    "       c            full command lines / names only\n"
    "       1            one line per CPU\n"
    "       k            send a signal to a process\n"
    "       d or s       change the delay\n"
    "       space        refresh now\n"
    "       Ctrl-Z       suspend (fg to come back)\n"
    "\nPress any key to continue";

enum class Sort { Cpu, Mem, Pid, Time, Rss, Name };

struct State {
  Sort sort = Sort::Cpu;
  bool reverse = false;
  bool full_cmd = false;
  bool per_cpu = false;
  double delay = 2.0;
  std::set<int> pids;
  bool by_user = false;
  uid_t uid = 0;
  std::string message;
} st;

void put(const char* p, size_t n) {
  while (n) {
    ssize_t w = write(1, p, n);
    if (w < 0 && errno == EINTR) continue;
    if (w <= 0) return;
    p += w;
    n -= static_cast<size_t>(w);
  }
}
void put(const std::string& s) { put(s.data(), s.size()); }

volatile sig_atomic_t g_winch = 0, g_quit = 0;
void on_winch(int) { g_winch = 1; }
void on_quit(int) { g_quit = 1; }

termios saved_tty;
bool tty_raw = false;

void term_restore() {
  if (!tty_raw) return;
  put("\x1b[?25h\x1b[?1049l");  // cursor on, back to the normal screen
  tcsetattr(0, TCSADRAIN, &saved_tty);
  tty_raw = false;
}

void term_raw() {
  if (tty_raw || tcgetattr(0, &saved_tty) != 0) return;
  termios t = saved_tty;
  t.c_lflag &= ~(ICANON | ECHO | ISIG);
  t.c_iflag &= ~(IXON | ICRNL);
  t.c_cc[VMIN] = 1;
  t.c_cc[VTIME] = 0;
  tcsetattr(0, TCSADRAIN, &t);
  put("\x1b[?1049h\x1b[?25l");  // alternate screen, cursor off
  tty_raw = true;
}

std::string mem_mib(uint64_t kb) {
  char b[32];
  std::snprintf(b, sizeof b, "%9.1f", static_cast<double>(kb) / 1024.0);
  return b;
}

// RES / VIRT like procps: KiB up to 99999, then m, then g.
std::string kib_short(uint64_t kb) {
  char b[32];
  if (kb < 100000) {
    std::snprintf(b, sizeof b, "%llu", static_cast<unsigned long long>(kb));
  } else if (kb < 100000ull * 1024) {
    std::snprintf(b, sizeof b, "%.1fm", static_cast<double>(kb) / 1024.0);
  } else {
    std::snprintf(b, sizeof b, "%.1fg", static_cast<double>(kb) / (1024.0 * 1024.0));
  }
  return b;
}

// TIME+ : minutes:seconds.hundredths
std::string time_plus(uint64_t ms) {
  char b[32];
  std::snprintf(b, sizeof b, "%llu:%02llu.%02llu", static_cast<unsigned long long>(ms / 60000),
                static_cast<unsigned long long>((ms / 1000) % 60), static_cast<unsigned long long>((ms / 10) % 100));
  return b;
}

std::string uptime_text(double secs) {
  long s = static_cast<long>(secs);
  long days = s / 86400, h = (s / 3600) % 24, m = (s / 60) % 60;
  char b[64];
  std::string out;
  if (days) out += std::to_string(days) + (days == 1 ? " day, " : " days, ");
  if (h) {
    std::snprintf(b, sizeof b, "%2ld:%02ld", h, m);
  } else {
    std::snprintf(b, sizeof b, "%ld min", m);
  }
  return out + b;
}

double mem_pct(const si::Process& p, const Snapshot& s) {
  uint64_t total = s.memory.total_kb() ? s.memory.total_kb() : s.processes.memory_total_kb();
  return total ? 100.0 * static_cast<double>(p.rss_kb()) / static_cast<double>(total) : 0.0;
}

std::vector<std::string> frame(const Snapshot& s, int width, int height, bool batch) {
  std::vector<std::string> L;
  char b[512];
  time_t now = time(nullptr);
  struct tm tm {};
  localtime_r(&now, &tm);
  char clock[16];
  strftime(clock, sizeof clock, "%H:%M:%S", &tm);
  std::snprintf(b, sizeof b, "top - %s up %s, load average: %.2f, %.2f, %.2f", clock,
                uptime_text(s.system.uptime_seconds()).c_str(), s.system.load1(), s.system.load5(), s.system.load15());
  L.push_back(b);

  int total = 0, running = 0, sleeping = 0, stopped = 0, zombie = 0;
  for (const si::Process& p : s.processes.processes()) {
    ++total;
    const std::string& c = p.state();
    if (c == "R") ++running;
    else if (c == "T" || c == "t") ++stopped;
    else if (c == "Z") ++zombie;
    else ++sleeping;
  }
  std::snprintf(b, sizeof b, "Tasks: %3d total, %3d running, %3d sleeping, %3d stopped, %3d zombie", total, running,
                sleeping, stopped, zombie);
  L.push_back(b);
  std::snprintf(b, sizeof b, "%%Cpu(s): %5.1f us, %5.1f sy, %5.1f wa, %5.1f id   (%u CPU%s, %s)", s.cpu.user_percent(),
                s.cpu.system_percent(), s.cpu.iowait_percent(), s.cpu.idle_percent(), s.cpu.count(),
                s.cpu.count() == 1 ? "" : "s", s.cpu.model().c_str());
  L.push_back(b);
  if (st.per_cpu) {
    for (int i = 0; i < s.cpu.per_cpu_percent_size(); ++i) {
      std::snprintf(b, sizeof b, "%%Cpu%-3d: %5.1f busy", i, s.cpu.per_cpu_percent(i));
      L.push_back(b);
    }
  }
  const si::MemoryReply& m = s.memory;
  std::snprintf(b, sizeof b, "MiB Mem : %s total, %s free, %s used, %s buff/cache", mem_mib(m.total_kb()).c_str(),
                mem_mib(m.free_kb()).c_str(), mem_mib(m.used_kb()).c_str(),
                mem_mib(m.buffers_kb() + m.cached_kb()).c_str());
  L.push_back(b);
  std::snprintf(b, sizeof b, "MiB Swap: %s total, %s free, %s used. %s avail Mem", mem_mib(m.swap_total_kb()).c_str(),
                mem_mib(m.swap_free_kb()).c_str(), mem_mib(m.swap_total_kb() - m.swap_free_kb()).c_str(),
                mem_mib(m.available_kb()).c_str());
  L.push_back(b);
  L.push_back(st.message);

  std::vector<const si::Process*> procs;
  for (const si::Process& p : s.processes.processes()) {
    if (!st.pids.empty() && !st.pids.count(p.pid())) continue;
    if (st.by_user && p.uid() != st.uid) continue;
    procs.push_back(&p);
  }
  auto key = [&](const si::Process* p) -> double {
    switch (st.sort) {
      case Sort::Cpu: return p->cpu_percent();
      case Sort::Mem: return mem_pct(*p, s);
      case Sort::Rss: return static_cast<double>(p->rss_kb());
      case Sort::Time: return static_cast<double>(p->cpu_time_ms());
      case Sort::Pid: return -static_cast<double>(p->pid());  // ascending pids by default
      case Sort::Name: return 0;
    }
    return 0;
  };
  std::stable_sort(procs.begin(), procs.end(), [&](const si::Process* a, const si::Process* b2) {
    bool less;
    if (st.sort == Sort::Name) {
      if (a->name() == b2->name()) return a->pid() < b2->pid();
      less = a->name() < b2->name();
      return st.reverse ? !less : less;
    }
    double x = key(a), y = key(b2);
    if (x == y) return a->pid() < b2->pid();
    return st.reverse ? x < y : x > y;
  });

  std::snprintf(b, sizeof b, "%7s %-8s %3s %7s %7s %s %5s %5s %9s %s", "PID", "USER", "NI", "RES", "VIRT", "S", "%CPU",
                "%MEM", "TIME+", "COMMAND");
  L.push_back(std::string(batch ? "" : "\x1b[7m") + b);
  size_t header_line = L.size() - 1;
  for (const si::Process* p : procs) {
    if (!batch && static_cast<int>(L.size()) >= height) break;
    std::string user = p->user().size() > 8 ? p->user().substr(0, 7) + "+" : p->user();
    std::snprintf(b, sizeof b, "%7d %-8s %3d %7s %7s %s %5.1f %5.1f %9s ", p->pid(), user.c_str(), p->nice(),
                  kib_short(p->rss_kb()).c_str(), kib_short(p->vsize_kb()).c_str(), p->state().c_str(),
                  p->cpu_percent(), mem_pct(*p, s), time_plus(p->cpu_time_ms()).c_str());
    L.push_back(b + command_text(*p, st.full_cmd));
  }
  if (width > 0) {
    for (size_t i = 0; i < L.size(); ++i) {
      std::string& line = L[i];
      size_t vis = i == header_line && !batch ? 4 : 0;  // the reverse-video escape isn't visible
      if (line.size() > static_cast<size_t>(width) + vis) line.resize(width + vis);
      if (i == header_line && !batch) {
        if (line.size() < static_cast<size_t>(width) + vis) line.append(width + vis - line.size(), ' ');
        line += "\x1b[0m";
      }
    }
  }
  return L;
}

std::string prompt(const std::string& question, int row) {
  // A line of input on the message row, echoed, in our raw mode.
  std::string ans;
  for (;;) {
    std::string o = "\x1b[" + std::to_string(row) + ";1H\x1b[K" + question + ans + "\x1b[?25h";
    put(o);
    char c;
    ssize_t n = read(0, &c, 1);
    if (n <= 0) break;
    if (c == '\r' || c == '\n') break;
    if (c == 27 || c == 3) {
      ans.clear();
      break;
    }
    if ((c == 127 || c == 8) && !ans.empty()) {
      ans.pop_back();
    } else if (c >= 32 && c < 127) {
      ans += c;
    }
  }
  put("\x1b[?25l");
  return ans;
}

void suspend() {
  term_restore();
  signal(SIGTSTP, SIG_DFL);
  raise(SIGTSTP);  // stopped here until fg
  term_raw();
}

}  // namespace

int main(int argc, char** argv) {
  bool batch = false;
  long count = -1;
  for (int i = 1; i < argc; ++i) {
    std::string a = argv[i];
    auto value = [&](const char* opt) -> std::string {
      if (a.size() > 2) return a.substr(2);
      if (i + 1 >= argc) {
        std::cerr << "top: " << opt << " needs an argument\n" << kUsage;
        std::exit(2);
      }
      return argv[++i];
    };
    if (a == "-h" || a == "--help") {
      std::cout << kUsage;
      return 0;
    } else if (a == "-v" || a == "--version") {
      std::cout << "top (myos) " MYOS_UTILS_VERSION "\n";
      return 0;
    } else if (a == "-b") {
      batch = true;
    } else if (a.rfind("-n", 0) == 0) {
      count = std::atol(value("-n").c_str());
    } else if (a.rfind("-d", 0) == 0) {
      st.delay = std::atof(value("-d").c_str());
      if (!(st.delay >= 0.1)) st.delay = 0.1;
    } else if (a.rfind("-o", 0) == 0) {
      std::string o = value("-o");
      if (!o.empty() && (o[0] == '%' || o[0] == '+' || o[0] == '-')) o = o.substr(1);
      if (o == "cpu" || o == "CPU") st.sort = Sort::Cpu;
      else if (o == "mem" || o == "MEM") st.sort = Sort::Mem;
      else if (o == "pid" || o == "PID") st.sort = Sort::Pid;
      else if (o == "time" || o == "TIME" || o == "TIME+") st.sort = Sort::Time;
      else if (o == "rss" || o == "res" || o == "RES") st.sort = Sort::Rss;
      else if (o == "name" || o == "command" || o == "COMMAND") st.sort = Sort::Name;
      else {
        std::cerr << "top: unknown sort field '" << o << "'\n" << kUsage;
        return 2;
      }
    } else if (a.rfind("-p", 0) == 0) {
      for (auto& p : split(value("-p"), ',')) {
        if (!p.empty()) st.pids.insert(std::atoi(p.c_str()));
      }
    } else if (a.rfind("-u", 0) == 0) {
      std::string u = value("-u");
      if (!lookup_user(u, st.uid)) {
        std::cerr << "top: unknown user '" << u << "'\n";
        return 1;
      }
      st.by_user = true;
    } else {
      std::cerr << "top: unexpected argument '" << a << "'\n" << kUsage;
      return 2;
    }
  }
  bool interactive = !batch && isatty(0) && isatty(1);
  SysinfoClient client;

  if (!interactive) {
    for (long n = 0; count < 0 || n < count; ++n) {
      if (n) {
        std::cout << "\n";
        usleep(static_cast<useconds_t>(st.delay * 1e6));
      }
      Snapshot s;
      try {
        s = client.snapshot();
      } catch (const myos::bus::Error& e) {
        std::cerr << "top: " << explain(e) << "\n";
        return 1;
      }
      int width = term_width(1);
      for (auto& line : frame(s, width, 0, true)) std::cout << line << "\n";
      std::cout << std::flush;
    }
    return 0;
  }

  signal(SIGWINCH, on_winch);
  signal(SIGTERM, on_quit);
  signal(SIGHUP, on_quit);
  signal(SIGINT, on_quit);
  term_raw();
  std::atexit(term_restore);
  long frames = 0;
  Snapshot s;
  bool have = false;
  bool fetch = true;
  bool help = false;
  for (;;) {
    if (g_quit) break;
    int width = term_width(1), height = term_height(1);
    if (fetch) {
      try {
        s = client.snapshot();
        have = true;
        if (st.message.rfind("sysinfo", 0) == 0 || st.message.rfind("can't", 0) == 0) st.message.clear();
      } catch (const myos::bus::Error& e) {
        st.message = explain(e);
      }
      ++frames;
    }
    fetch = true;
    std::string out = "\x1b[H";
    if (help) {
      out += "\x1b[2J";
      out += std::string("top (myos) ") + MYOS_UTILS_VERSION + "\r\n\r\n";
      for (const std::string& l : split(kHelp, '\n')) out += l + "\r\n";
    } else if (have) {
      std::vector<std::string> lines = frame(s, width, height, false);
      for (size_t i = 0; i < lines.size(); ++i) out += lines[i] + "\x1b[K" + (i + 1 < lines.size() ? "\r\n" : "");
      out += "\x1b[J";
    } else {
      out += "\x1b[2J" + st.message;
    }
    put(out);
    if (count > 0 && frames >= count) break;

    pollfd p{0, POLLIN, 0};
    int r = poll(&p, 1, static_cast<int>(st.delay * 1000));
    if (r < 0 && errno == EINTR) {
      if (g_winch) {
        g_winch = 0;
        fetch = false;
      }
      continue;
    }
    if (r == 0) continue;
    char c;
    if (read(0, &c, 1) != 1) break;
    if (help) {
      help = false;
      fetch = false;
      put("\x1b[2J");
      continue;
    }
    switch (c) {
      case 'q':
      case 3:
        g_quit = 1;
        break;
      case 'P': st.sort = Sort::Cpu; break;
      case 'M': st.sort = Sort::Mem; break;
      case 'N': st.sort = Sort::Pid; break;
      case 'T': st.sort = Sort::Time; break;
      case 'R': st.reverse = !st.reverse; break;
      case 'c': st.full_cmd = !st.full_cmd; break;
      case '1': st.per_cpu = !st.per_cpu; break;
      case 'h':
      case '?':
        help = true;
        fetch = false;
        break;
      case 12:  // Ctrl-L
        put("\x1b[2J");
        fetch = false;
        break;
      case 26:  // Ctrl-Z
        suspend();
        break;
      case 'k': {
        int row = 7 + (st.per_cpu ? s.cpu.per_cpu_percent_size() : 0);
        std::string pid = prompt("PID to signal/kill: ", row);
        if (pid.empty()) break;
        std::string sig = prompt("Send pid " + pid + " signal [15/TERM]: ", row);
        int signo = SIGTERM;
        if (!sig.empty()) {
          signo = std::isdigit(static_cast<unsigned char>(sig[0])) ? std::atoi(sig.c_str()) : -1;
          static const std::pair<const char*, int> names[] = {{"HUP", SIGHUP}, {"INT", SIGINT}, {"KILL", SIGKILL},
                                                              {"TERM", SIGTERM}, {"STOP", SIGSTOP}, {"CONT", SIGCONT},
                                                              {"USR1", SIGUSR1}, {"USR2", SIGUSR2}};
          std::string n = sig.rfind("SIG", 0) == 0 ? sig.substr(3) : sig;
          for (char& ch : n) ch = static_cast<char>(std::toupper(static_cast<unsigned char>(ch)));
          for (auto& [name, num] : names) {
            if (n == name) signo = num;
          }
        }
        if (signo <= 0) {
          st.message = "unknown signal '" + sig + "'";
        } else if (kill(std::atoi(pid.c_str()), signo) != 0) {
          st.message = "kill " + pid + ": " + std::strerror(errno);
        } else {
          st.message.clear();
        }
        break;
      }
      case 'd':
      case 's': {
        int row = 7 + (st.per_cpu ? s.cpu.per_cpu_percent_size() : 0);
        char cur[32];
        std::snprintf(cur, sizeof cur, "%.1f", st.delay);
        std::string v = prompt(std::string("Change delay from ") + cur + " to: ", row);
        if (!v.empty()) {
          double d = std::atof(v.c_str());
          if (d >= 0.1) st.delay = d;
          else st.message = "delay must be at least 0.1 s";
        }
        break;
      }
      default:
        break;  // space, Enter: refresh now
    }
  }
  term_restore();
  return 0;
}
