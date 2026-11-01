// msh: the myos shell.
//
//   msh                    interactive (when stdin is a terminal), else reads commands from stdin
//   msh -c 'cmds' [name [args...]]
//   msh script [args...]
//   msh -n ...             parse only: report syntax errors, run nothing
//
// A login shell (argv[0] starting with '-', or -l) and every interactive
// shell read /etc/mshrc and then ~/.mshrc.

#include <fcntl.h>
#include <pwd.h>
#include <signal.h>
#include <unistd.h>

#include <cstring>
#include <iostream>

#include "complete.hpp"
#include "lineedit.hpp"
#include "shell.hpp"

#ifndef MSH_VERSION
#define MSH_VERSION "0.1.0"
#endif

namespace msh {
namespace {

const char* kUsage =
    "usage: msh [-ilnex] [-c command [name [arg...]] | script [arg...]]\n"
    "  -c  run command and exit      -i  interactive even if stdin isn't a terminal\n"
    "  -l  login shell               -n  check syntax only\n"
    "  -e  exit on the first error   -x  print commands as they run\n";

void on_signal(int sig) {
  if (sig == SIGINT) g_sigint = 1;
  if (sig == SIGHUP) g_sighup = 1;
  if (sig == SIGWINCH) g_sigwinch = 1;
}

void catch_signal(int sig) {
  struct sigaction sa {};
  sa.sa_handler = on_signal;
  sigemptyset(&sa.sa_mask);
  sa.sa_flags = 0;  // no SA_RESTART: blocking reads and waits notice the signal
  sigaction(sig, &sa, nullptr);
}

}  // namespace

void Shell::init_interactive() {
  catch_signal(SIGINT);
  catch_signal(SIGHUP);
  catch_signal(SIGWINCH);
  signal(SIGQUIT, SIG_IGN);
  signal(SIGTERM, SIG_IGN);
  if (!isatty(0)) return;
  tty = fcntl(0, F_DUPFD_CLOEXEC, 10);
  if (tty < 0 || tcgetpgrp(tty) < 0) return;  // a terminal, but not our controlling one
  // Started in the background? Wait until we're brought to the foreground.
  for (;;) {
    pid_t fg = tcgetpgrp(tty);
    pgid = getpgrp();
    if (fg == pgid || fg < 0) break;
    kill(-pgid, SIGTTIN);
  }
  signal(SIGTSTP, SIG_IGN);
  signal(SIGTTIN, SIG_IGN);
  signal(SIGTTOU, SIG_IGN);
  pid_t me = getpid();
  if (pgid != me && setpgid(me, me) == 0) pgid = me;  // a session leader already leads its group
  tcsetpgrp(tty, pgid);
  tcgetattr(tty, &tmodes);
  job_control = true;
}

namespace {

// !! !n !-n !prefix !$ in an interactive line. Returns false (after an error
// message) if an event isn't found.
bool expand_history(std::string& line, const History& h, bool& changed) {
  const auto& e = h.entries();
  std::string out;
  changed = false;
  char quote = 0;
  for (size_t i = 0; i < line.size(); ++i) {
    char c = line[i];
    if (quote == '\'') {
      if (c == '\'') quote = 0;
      out += c;
      continue;
    }
    if (c == '\\' && i + 1 < line.size()) {
      out += c;
      out += line[++i];
      continue;
    }
    if (c == '\'' && !quote) quote = '\'';
    else if (c == '"') quote = quote ? 0 : '"';
    if (c != '!' || i + 1 >= line.size()) {
      out += c;
      continue;
    }
    char n = line[i + 1];
    std::string ev;
    size_t len = 0;
    if (n == '!') {
      if (e.empty()) {
        std::cerr << "msh: !!: event not found" << std::endl;
        return false;
      }
      ev = e.back();
      len = 2;
    } else if (n == '$') {
      if (e.empty()) {
        std::cerr << "msh: !$: event not found" << std::endl;
        return false;
      }
      std::string last = e.back();
      size_t end = last.find_last_not_of(" \t");
      size_t b = last.find_last_of(" \t", end);
      ev = last.substr(b == std::string::npos ? 0 : b + 1, end - (b == std::string::npos ? 0 : b + 1) + 1);
      len = 2;
    } else if (std::isdigit(static_cast<unsigned char>(n)) || (n == '-' && i + 2 < line.size() && std::isdigit(static_cast<unsigned char>(line[i + 2])))) {
      size_t j = i + 2;
      while (j < line.size() && std::isdigit(static_cast<unsigned char>(line[j]))) ++j;
      long k = std::stol(line.substr(i + 1, j - i - 1));
      long idx = k < 0 ? static_cast<long>(e.size()) + k : k - 1;
      if (idx < 0 || idx >= static_cast<long>(e.size())) {
        std::cerr << "msh: " << line.substr(i, j - i) << ": event not found" << std::endl;
        return false;
      }
      ev = e[idx];
      len = j - i;
    } else if (!std::strchr(" \t\n=(\"'", n)) {
      size_t j = i + 1;
      while (j < line.size() && !std::strchr(" \t\n;&|<>()\"'", line[j])) ++j;
      std::string prefix = line.substr(i + 1, j - i - 1);
      long idx = -1;
      for (long k = static_cast<long>(e.size()) - 1; k >= 0; --k) {
        if (e[k].compare(0, prefix.size(), prefix) == 0) {
          idx = k;
          break;
        }
      }
      if (idx < 0) {
        std::cerr << "msh: !" << prefix << ": event not found" << std::endl;
        return false;
      }
      ev = e[idx];
      len = j - i;
    } else {
      out += c;
      continue;
    }
    out += ev;
    i += len - 1;
    changed = true;
  }
  line = out;
  return true;
}

// One history entry for a command typed over several lines.
std::string history_form(const std::string& src) {
  std::string out;
  size_t b = 0;
  while (b <= src.size()) {
    size_t e = src.find('\n', b);
    std::string ln = src.substr(b, e == std::string::npos ? std::string::npos : e - b);
    size_t f = ln.find_first_not_of(" \t");
    ln = f == std::string::npos ? "" : ln.substr(f);
    if (!out.empty() && !ln.empty()) {
      std::string t = out;
      while (!t.empty() && (t.back() == ' ' || t.back() == '\t')) t.pop_back();
      size_t sp = t.find_last_of(" \t;");
      std::string last = sp == std::string::npos ? t : t.substr(sp + 1);
      bool glue = last == "then" || last == "do" || last == "else" || last == "{" || last == "in" ||
                  (!t.empty() && std::strchr("|&;(", t.back()));
      out = t + (glue ? " " : "; ");
    }
    out += ln;
    if (e == std::string::npos) break;
    b = e + 1;
  }
  return out;
}

int interactive_loop(Shell& sh) {
  History hist;
  LineEditor ed(0, 2, hist);
  std::string home = sh.get_or("HOME");
  if (home.empty()) {
    if (const passwd* pw = getpwuid(getuid())) home = pw->pw_dir;
  }
  std::string histfile = sh.get_or("HISTFILE", home.empty() ? "" : home + "/.msh_history");
  size_t histsize = 1000;
  try {
    histsize = std::stoul(sh.get_or("HISTSIZE", "1000"));
  } catch (...) {
  }
  ed.detect_size();
  hist.load(histfile, histsize);
  ed.completer = [&sh](const std::string& line, size_t cur) { return complete_line(sh, line, cur); };
  sh.history_list = [&hist]() { return hist.entries(); };
  sh.history_clear = [&hist]() { hist.clear(); };
  sh.resize = [&ed]() { ed.detect_size(); };
  sh.on_exit = [&sh]() {
    for (Job& j : sh.jobs) {  // like a hangup: don't leave stopped jobs behind
      if (j.stopped()) {
        kill(-j.pgid, SIGHUP);
        kill(-j.pgid, SIGCONT);
      }
    }
  };

  sh.run_file("/etc/mshrc", false);
  if (!home.empty()) sh.run_file(home + "/.mshrc", false);
  sh.exiting = false;

  for (;;) {
    if (g_sighup) break;
    sh.notify_jobs();
    g_sigint = 0;
    sh.interrupted = false;
    sh.break_levels = sh.continue_levels = 0;
    std::string prompt = sh.expand_prompt(sh.get("PS1").value_or("\\u@\\h:\\w\\$ "));
    std::string line;
    LineEditor::Status st = ed.read_line(prompt, line);
    if (g_sighup) break;
    if (st == LineEditor::Status::Eof) {
      std::vector<std::string> exit_argv = {"exit"};
      builtins().at("exit")(sh, exit_argv);
      if (sh.exiting) break;
      continue;
    }
    if (st == LineEditor::Status::Interrupted) {
      sh.last_status = 130;
      continue;
    }
    bool changed = false;
    if (!expand_history(line, hist, changed)) {
      sh.last_status = 1;
      continue;
    }
    std::string src = line;
    bool cancelled = false;
    for (;;) {
      try {
        parse(src, &sh.aliases);
        break;
      } catch (const Incomplete&) {
        std::string more;
        LineEditor::Status s2 = ed.read_line(sh.expand_prompt(sh.get("PS2").value_or("> ")), more);
        if (s2 != LineEditor::Status::Ok) {
          if (s2 == LineEditor::Status::Eof) sh.err("syntax error: unexpected end of file");
          cancelled = true;
          break;
        }
        bool c2 = false;
        if (!expand_history(more, hist, c2)) {
          cancelled = true;
          break;
        }
        changed = changed || c2;
        src += "\n" + more;
      } catch (const SyntaxError&) {
        break;  // run_string reports it
      }
    }
    if (cancelled) {
      hist.add(history_form(src));
      continue;
    }
    if (changed) write_all(2, src + "\n");
    hist.add(history_form(src));
    if (src.find_first_not_of(" \t\n") != std::string::npos && src.find("exit") == std::string::npos) {
      sh.warned_stopped = false;
    }
    sh.run_string(src);
    // Ctrl-C while the shell itself was busy (a loop with no child to
    // report it): the terminal echoed ^C, so start the prompt on a new line.
    if (g_sigint) write_all(2, "\n");
    if (sh.exiting) break;
  }
  if (g_sighup) {
    for (Job& j : sh.jobs) {
      kill(-j.pgid, SIGHUP);
      if (j.stopped()) kill(-j.pgid, SIGCONT);
    }
    sh.exit_now(129);
  }
  sh.exit_now(sh.exiting ? sh.exit_code : sh.last_status);
}

// Commands from a non-terminal stdin, one complete command at a time, read
// a byte at a time so that commands like `read` get the rest of the input.
int stdin_loop(Shell& sh) {
  std::string src, line;
  bool eof = false;
  while (!eof) {
    line.clear();
    char c;
    for (;;) {
      ssize_t n = read(0, &c, 1);
      if (n < 0 && errno == EINTR) continue;
      if (n <= 0) {
        eof = true;
        break;
      }
      if (c == '\n') break;
      line += c;
    }
    if (eof && line.empty() && src.empty()) break;
    src += src.empty() ? line : "\n" + line;
    try {
      parse(src, &sh.aliases);
    } catch (const Incomplete&) {
      if (!eof) continue;
    } catch (const SyntaxError&) {
    }
    sh.run_string(src);
    src.clear();
    if (sh.exiting) break;
  }
  return sh.exiting ? sh.exit_code : sh.last_status;
}

}  // namespace
}  // namespace msh

int main(int argc, char** argv) {
  using namespace msh;
  Shell sh;
  sh.import_environment();
  sh.login = argv[0] && argv[0][0] == '-';
  sh.arg0 = argv[0] ? argv[0] : "msh";  // "-msh" for a login shell, as bash keeps "-bash"
  bool force_interactive = false, parse_only = false;
  const char* command = nullptr;
  int i = 1;
  for (; i < argc && argv[i][0] == '-' && argv[i][1]; ++i) {
    std::string a = argv[i];
    if (a == "--") {
      ++i;
      break;
    }
    if (a == "--help") {
      std::cout << kUsage;
      return 0;
    }
    if (a == "--version") {
      std::cout << "msh (myos) " MSH_VERSION << std::endl;
      return 0;
    }
    if (a == "--login") {
      sh.login = true;
      continue;
    }
    for (size_t k = 1; k < a.size(); ++k) {
      switch (a[k]) {
        case 'c':
          if (i + 1 >= argc) {
            std::cerr << "msh: -c: option requires an argument\n" << kUsage;
            return 2;
          }
          command = argv[++i];
          break;
        case 'i': force_interactive = true; break;
        case 'l': sh.login = true; break;
        case 'n': parse_only = true; break;
        case 'e': sh.opt_errexit = true; break;
        case 'x': sh.opt_xtrace = true; break;
        case 's': break;
        default:
          std::cerr << "msh: -" << a[k] << ": invalid option\n" << kUsage;
          return 2;
      }
    }
    if (command) {
      ++i;
      break;
    }
  }
  signal(SIGPIPE, SIG_DFL);

  if (command) {
    if (i < argc) sh.arg0 = argv[i++];
    sh.positional.assign(argv + i, argv + argc);
    if (parse_only) {
      try {
        parse(command);
        return 0;
      } catch (const std::exception& e) {
        sh.err(e.what());
        return 2;
      }
    }
    int st = sh.run_string(command);
    sh.exit_now(sh.exiting ? sh.exit_code : st);
  }
  if (i < argc) {
    std::string path = argv[i];
    sh.arg0 = path;
    sh.positional.assign(argv + i + 1, argv + argc);
    if (access(path.c_str(), R_OK) != 0) {
      sh.err(path + ": " + std::strerror(errno));
      return 127;
    }
    if (parse_only) {
      FILE* f = fopen(path.c_str(), "r");
      std::string src;
      char buf[4096];
      size_t n;
      while (f && (n = fread(buf, 1, sizeof buf, f)) > 0) src.append(buf, n);
      if (f) fclose(f);
      try {
        parse(src);
        return 0;
      } catch (const std::exception& e) {
        sh.err(path + ": " + e.what());
        return 2;
      }
    }
    int st = sh.run_file(path);
    sh.exit_now(sh.exiting ? sh.exit_code : st);
  }
  if (force_interactive || (isatty(0) && isatty(2))) {
    sh.interactive = true;
    sh.init_interactive();
    return interactive_loop(sh);
  }
  sh.exit_now(stdin_loop(sh));
}
