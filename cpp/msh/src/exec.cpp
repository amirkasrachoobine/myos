// Running commands: lists, and-or lists, pipelines, simple and compound
// commands, redirections, $(...), and finding programs on PATH.

#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cstring>
#include <fstream>
#include <sstream>

#include "shell.hpp"

extern char** environ;

namespace msh {

volatile sig_atomic_t g_sigint = 0;
volatile sig_atomic_t g_sighup = 0;
volatile sig_atomic_t g_sigwinch = 0;

std::string join(const std::vector<std::string>& v, const std::string& sep) {
  std::string s;
  for (size_t i = 0; i < v.size(); ++i) {
    if (i) s += sep;
    s += v[i];
  }
  return s;
}

void write_all(int fd, const std::string& s) {
  size_t off = 0;
  while (off < s.size()) {
    ssize_t n = write(fd, s.data() + off, s.size() - off);
    if (n < 0) {
      if (errno == EINTR) continue;
      return;
    }
    off += static_cast<size_t>(n);
  }
}

Shell::Shell() { top_pid = getpid(); }

void Shell::err(const std::string& msg) const { write_all(2, "msh: " + msg + "\n"); }

// --- variables ---------------------------------------------------------------

std::optional<std::string> Shell::get(const std::string& name) const {
  auto it = vars.find(name);
  if (it == vars.end()) return std::nullopt;
  return it->second.value;
}

std::string Shell::get_or(const std::string& name, const std::string& def) const {
  auto it = vars.find(name);
  return it == vars.end() ? def : it->second.value;
}

void Shell::set(const std::string& name, const std::string& value, bool exp) {
  Var& v = vars[name];
  v.value = value;
  v.exported = v.exported || exp;
}

void Shell::unset(const std::string& name) { vars.erase(name); }

void Shell::import_environment() {
  for (char** e = environ; e && *e; ++e) {
    const char* eq = std::strchr(*e, '=');
    if (!eq) continue;
    std::string name(*e, eq - *e);
    if (is_name(name)) vars[name] = Var{eq + 1, true};
  }
  if (!get("PATH")) set("PATH", "/bin:/sbin:/usr/bin:/usr/sbin", true);
  char buf[4096];
  if (getcwd(buf, sizeof buf)) {
    struct stat a, b;
    auto pwd = get("PWD");
    if (!pwd || stat(pwd->c_str(), &a) != 0 || stat(buf, &b) != 0 || a.st_ino != b.st_ino || a.st_dev != b.st_dev) {
      set("PWD", buf, true);
    }
  }
  long lvl = 0;
  try {
    lvl = std::stol(get_or("SHLVL", "0"));
  } catch (...) {
  }
  set("SHLVL", std::to_string(lvl + 1), true);
}

// --- running -------------------------------------------------------------------

bool Shell::unwinding() const { return break_levels || continue_levels || returning || exiting || interrupted; }

void Shell::exit_now(int code) {
  if (in_child) _exit(code & 0xff);
  if (on_exit) on_exit();
  _exit(code & 0xff);
}

int Shell::run_string(const std::string& src, const std::string& origin) {
  NodeP n;
  try {
    n = parse(src, &aliases);
  } catch (const Incomplete&) {
    err((origin.empty() ? "" : origin + ": ") + "syntax error: unexpected end of file");
    return last_status = 2;
  } catch (const SyntaxError& e) {
    err((origin.empty() ? "" : origin + ": ") + e.what());
    return last_status = 2;
  }
  return run(n);
}

int Shell::run_file(const std::string& path, bool must_exist) {
  std::ifstream f(path);
  if (!f) {
    if (must_exist) err(path + ": " + std::strerror(errno));
    return must_exist ? 1 : 0;
  }
  std::stringstream ss;
  ss << f.rdbuf();
  std::string src = ss.str();
  if (src.compare(0, 2, "#!") == 0) src.erase(0, src.find('\n'));
  return run_string(src, path);
}

void Shell::check_errexit(int status) {
  if (opt_errexit && status != 0 && cond_depth == 0 && !interrupted) {
    exiting = true;
    exit_code = status;
  }
}

int Shell::run(const NodeP& n) {
  if (!n) return 0;
  if (g_sigint && interactive) interrupted = true;
  if (unwinding()) return last_status;
  int st = 0;
  switch (n->kind) {
    case Node::List:
      st = run_list(*n);
      break;
    case Node::AndOr:
      st = run_and_or(*n);
      break;
    case Node::Pipeline:
      st = run_pipeline(*n);
      break;
    case Node::Simple:
      st = run_simple(*n, false);
      check_errexit(st);
      break;
    case Node::Subshell:
      st = launch({Launch{n, {}, {}, {}}}, false, n->text);
      check_errexit(st);
      break;
    case Node::FuncDef:
      functions[n->name] = n->body;
      st = 0;
      break;
    default:
      st = run_compound(*n);
  }
  return last_status = st;
}

int Shell::run_list(const Node& n) {
  int st = last_status;
  for (size_t i = 0; i < n.kids.size(); ++i) {
    const NodeP& kid = n.kids[i];
    if (n.background[i]) {
      std::vector<Launch> cmds;
      if (kid->kids.size() == 1 && !kid->kids[0]->negate) {
        for (const NodeP& c : kid->kids[0]->kids) cmds.push_back(Launch{c, {}, {}, {}});
      } else {
        cmds.push_back(Launch{kid, {}, {}, {}});
      }
      st = last_status = launch(std::move(cmds), true, kid->text);
    } else {
      st = run(kid);
    }
    if (g_sigint && interactive) interrupted = true;
    if (unwinding()) break;
  }
  return st;
}

int Shell::run_and_or(const Node& n) {
  size_t last = n.kids.size() - 1;
  if (last) ++cond_depth;
  int st = run(n.kids[0]);
  if (last) --cond_depth;
  for (size_t i = 1; i <= last && !unwinding(); ++i) {
    bool go = n.ops[i - 1] == "&&" ? st == 0 : st != 0;
    if (!go) continue;
    if (i < last) ++cond_depth;
    st = run(n.kids[i]);
    if (i < last) --cond_depth;
  }
  return st;
}

int Shell::run_pipeline(const Node& n) {
  if (n.negate) ++cond_depth;
  int st;
  if (n.kids.size() == 1) {
    st = run(n.kids[0]);
  } else {
    std::vector<Launch> cmds;
    for (const NodeP& c : n.kids) cmds.push_back(Launch{c, {}, {}, {}});
    st = launch(std::move(cmds), false, n.text);
  }
  if (n.negate) {
    --cond_depth;
    st = st == 0 ? 1 : 0;
  } else if (n.kids.size() > 1) {
    check_errexit(st);
  }
  return st;
}

int Shell::run_compound(const Node& n) {
  std::vector<std::pair<int, int>> saved;
  if (!apply_redirs(n.redirs, &saved)) {
    restore_redirs(saved);
    return 1;
  }
  int st = 0;
  auto loop_control = [&]() -> bool {  // true: leave this loop
    if (break_levels > 0) {
      --break_levels;
      return true;
    }
    if (continue_levels > 0) {
      if (--continue_levels > 0) return true;
      return false;
    }
    return unwinding();
  };
  switch (n.kind) {
    case Node::Group:
      st = run(n.body);
      break;
    case Node::If: {
      bool taken = false;
      for (size_t i = 0; i < n.conds.size() && !taken; ++i) {
        ++cond_depth;
        int c = run(n.conds[i]);
        --cond_depth;
        if (unwinding()) break;
        if (c == 0) {
          taken = true;
          st = run(n.bodies[i]);
        }
      }
      if (!taken && !unwinding()) st = n.else_body ? run(n.else_body) : 0;
      break;
    }
    case Node::While:
      ++loop_depth;
      for (;;) {
        ++cond_depth;
        int c = run(n.cond);
        --cond_depth;
        if (unwinding() || (n.until ? c == 0 : c != 0)) {
          if (break_levels > 0) --break_levels;
          break;
        }
        st = run(n.body);
        if (loop_control()) break;
      }
      --loop_depth;
      break;
    case Node::For: {
      std::vector<std::string> items;
      try {
        items = n.has_in ? expand_words(n.items) : positional;
      } catch (const ExpandError& e) {
        err(e.what());
        st = 1;
        break;
      }
      ++loop_depth;
      for (const std::string& item : items) {
        set(n.name, item);
        st = run(n.body);
        if (loop_control()) break;
      }
      --loop_depth;
      break;
    }
    default:
      break;
  }
  restore_redirs(saved);
  return st;
}

int Shell::run_function(const NodeP& body, std::vector<std::string>& argv) {
  if (func_depth > 200) {
    err(argv[0] + ": maximum function nesting level exceeded");
    return 1;
  }
  std::vector<std::string> saved_pos = positional;
  positional.assign(argv.begin() + 1, argv.end());
  ++func_depth;
  locals.emplace_back();
  int st = run(body);
  for (auto it = locals.back().rbegin(); it != locals.back().rend(); ++it) {
    if (it->second) {
      vars[it->first] = *it->second;
    } else {
      vars.erase(it->first);
    }
  }
  locals.pop_back();
  --func_depth;
  positional = saved_pos;
  if (returning) {
    returning = false;
    st = last_status;
  }
  return st;
}

int Shell::run_simple(const Node& n, bool child) {
  std::vector<std::string> argv;
  std::vector<std::pair<std::string, std::string>> assigns;
  subst_status = -1;
  try {
    for (const std::string& a : n.assigns) {
      size_t eq = a.find('=');
      assigns.emplace_back(a.substr(0, eq), expand_string(a.substr(eq + 1)));
    }
    argv = expand_words(n.words);
  } catch (const ExpandError& e) {
    err(e.what());
    if (!interactive) {
      exiting = true;
      exit_code = 1;
    }
    return 1;
  }
  if (interrupted) return 130;
  if (opt_xtrace) {
    std::string t = "+";
    for (auto& a : assigns) t += " " + a.first + "=" + a.second;
    for (auto& a : argv) t += " " + a;
    write_all(2, t + "\n");
  }

  if (argv.empty()) {
    for (auto& a : assigns) set(a.first, a.second);
    std::vector<std::pair<int, int>> saved;
    bool ok = apply_redirs(n.redirs, &saved);
    restore_redirs(saved);
    if (!ok) return 1;
    return subst_status >= 0 ? subst_status : 0;
  }

  const std::string& name = argv[0];
  if (name == "exec") {
    if (!apply_redirs(n.redirs, nullptr)) return 1;
    if (argv.size() == 1) return 0;
    argv.erase(argv.begin());
    if (job_control) tcsetattr(tty, TCSADRAIN, &tmodes);
    if (on_exit) on_exit();
    exec_external(argv, assigns);
  }

  auto fn = functions.find(name);
  auto bi = builtins().find(name);
  if (fn != functions.end() || bi != builtins().end()) {
    // Assignments before a function or builtin last for that command only.
    std::vector<std::pair<std::string, std::optional<Var>>> old;
    for (auto& a : assigns) {
      auto it = vars.find(a.first);
      old.emplace_back(a.first, it == vars.end() ? std::nullopt : std::optional<Var>(it->second));
      set(a.first, a.second, true);
    }
    std::vector<std::pair<int, int>> saved;
    int st;
    if (!apply_redirs(n.redirs, child ? nullptr : &saved)) {
      st = 1;
    } else if (fn != functions.end()) {
      NodeP body = fn->second;  // keep it alive if the function redefines itself
      st = run_function(body, argv);
    } else {
      st = bi->second(*this, argv);
    }
    restore_redirs(saved);
    for (auto& o : old) {
      if (o.second) {
        vars[o.first] = *o.second;
      } else {
        vars.erase(o.first);
      }
    }
    return st;
  }

  if (child) {
    if (!apply_redirs(n.redirs, nullptr)) _exit(1);
    exec_external(argv, assigns);
  }
  return launch({Launch{nullptr, argv, assigns, n.redirs}}, false, n.text);
}

std::string Shell::find_command(const std::string& name) const {
  if (name.empty()) return "";
  if (name.find('/') != std::string::npos) return name;
  std::string path = get_or("PATH");
  size_t start = 0;
  for (;;) {
    size_t colon = path.find(':', start);
    std::string dir = path.substr(start, colon == std::string::npos ? std::string::npos : colon - start);
    if (dir.empty()) dir = ".";
    std::string cand = dir + "/" + name;
    struct stat st;
    if (stat(cand.c_str(), &st) == 0 && S_ISREG(st.st_mode) && access(cand.c_str(), X_OK) == 0) return cand;
    if (colon == std::string::npos) break;
    start = colon + 1;
  }
  return "";
}

void Shell::exec_external(std::vector<std::string>& argv,
                          const std::vector<std::pair<std::string, std::string>>& env) {
  std::map<std::string, std::string> e;
  for (auto& [k, v] : vars) {
    if (v.exported) e[k] = v.value;
  }
  for (auto& [k, v] : env) e[k] = v;
  std::vector<std::string> envs;
  for (auto& [k, v] : e) envs.push_back(k + "=" + v);
  std::vector<char*> envp, args;
  for (auto& s : envs) envp.push_back(s.data());
  envp.push_back(nullptr);
  for (auto& s : argv) args.push_back(s.data());
  args.push_back(nullptr);

  std::string path = find_command(argv[0]);
  if (path.empty()) {
    err(argv[0] + ": command not found");
    _exit(127);
  }
  execve(path.c_str(), args.data(), envp.data());
  int e1 = errno;
  if (e1 == ENOEXEC) {  // a script without #!: run it with /bin/sh
    std::vector<char*> sh = {const_cast<char*>("/bin/sh"), const_cast<char*>(path.c_str())};
    for (size_t i = 1; i < argv.size(); ++i) sh.push_back(argv[i].data());
    sh.push_back(nullptr);
    execve("/bin/sh", sh.data(), envp.data());
  }
  struct stat st;
  if (stat(path.c_str(), &st) == 0 && S_ISDIR(st.st_mode)) {
    err(path + ": Is a directory");
    _exit(126);
  }
  err(path + ": " + std::strerror(e1));
  _exit(e1 == ENOENT ? 127 : 126);
}

// --- child processes -------------------------------------------------------------

void Shell::child_setup(pid_t job_pgid, bool foreground) {
  in_child = true;
  if (job_control) {
    pid_t me = getpid();
    pid_t pg = job_pgid ? job_pgid : me;
    setpgid(me, pg);
    if (foreground) tcsetpgrp(tty, pg);
  }
  for (int sig : {SIGINT, SIGQUIT, SIGTSTP, SIGTTIN, SIGTTOU, SIGHUP, SIGTERM, SIGWINCH, SIGPIPE}) {
    signal(sig, SIG_DFL);
  }
  if (!job_control && !foreground) {
    signal(SIGINT, SIG_IGN);
    signal(SIGQUIT, SIG_IGN);
  }
  sigset_t none;
  sigemptyset(&none);
  sigprocmask(SIG_SETMASK, &none, nullptr);
  job_control = false;
  interactive = false;
  jobs.clear();
  job_order.clear();
}

void Shell::child_run(Launch& l) {
  int st = 0;
  try {
    if (!l.node) {
      if (!apply_redirs(l.redirs, nullptr)) _exit(1);
      exec_external(l.argv, l.env);
    }
    const Node& n = *l.node;
    if (n.kind == Node::Simple) {
      st = run_simple(n, true);
    } else if (n.kind == Node::Subshell) {
      if (!apply_redirs(n.redirs, nullptr)) _exit(1);
      st = run(n.body);
    } else {
      st = run(l.node);
    }
  } catch (const std::exception& e) {
    err(e.what());
    st = 1;
  }
  exit_now(exiting ? exit_code : st);
}

int Shell::launch(std::vector<Launch> cmds, bool background, const std::string& text) {
  Job job;
  job.text = text;
  job.background = background;
  int prev_read = -1;
  for (size_t i = 0; i < cmds.size(); ++i) {
    int pfd[2] = {-1, -1};
    if (i + 1 < cmds.size() && pipe2(pfd, O_CLOEXEC) != 0) {
      err(std::string("pipe: ") + std::strerror(errno));
      break;
    }
    pid_t pid = fork();
    if (pid < 0) {
      err(std::string("fork: ") + std::strerror(errno));
      if (pfd[0] >= 0) {
        close(pfd[0]);
        close(pfd[1]);
      }
      break;
    }
    if (pid == 0) {
      child_setup(job.pgid, !background);
      if (prev_read >= 0) {
        dup2(prev_read, 0);
        close(prev_read);
      }
      if (pfd[1] >= 0) {
        dup2(pfd[1], 1);
        close(pfd[1]);
        close(pfd[0]);
      }
      child_run(cmds[i]);
    }
    if (!job.pgid) job.pgid = pid;
    if (job_control) setpgid(pid, job.pgid);
    job.procs.push_back(Proc{pid});
    if (prev_read >= 0) close(prev_read);
    if (pfd[1] >= 0) close(pfd[1]);
    prev_read = pfd[0];
  }
  if (prev_read >= 0) close(prev_read);
  if (job.procs.empty()) return 1;

  if (background) {
    last_bg = job.procs.back().pid;
    int id = add_job(std::move(job));
    if (interactive) write_all(2, "[" + std::to_string(id) + "] " + std::to_string(last_bg) + "\n");
    return 0;
  }
  if (!job_control) {
    for (Proc& p : job.procs) {
      int st;
      while (waitpid(p.pid, &st, 0) < 0 && errno == EINTR) {
      }
      p.status = st;
      p.done = true;
    }
    return job.status();
  }
  int id = add_job(std::move(job));
  return wait_foreground(*job_by_id(id));
}

std::string Shell::command_output(const std::string& src) {
  int pfd[2];
  if (pipe2(pfd, O_CLOEXEC) != 0) {
    err(std::string("pipe: ") + std::strerror(errno));
    return "";
  }
  pid_t pid = fork();
  if (pid < 0) {
    err(std::string("fork: ") + std::strerror(errno));
    close(pfd[0]);
    close(pfd[1]);
    return "";
  }
  if (pid == 0) {
    in_child = true;
    signal(SIGINT, SIG_DFL);
    signal(SIGQUIT, SIG_DFL);
    signal(SIGHUP, SIG_DFL);
    signal(SIGTERM, SIG_DFL);
    signal(SIGWINCH, SIG_DFL);
    job_control = false;
    interactive = false;
    jobs.clear();
    job_order.clear();
    dup2(pfd[1], 1);
    close(pfd[1]);
    close(pfd[0]);
    int st = run_string(src);
    exit_now(exiting ? exit_code : st);
  }
  close(pfd[1]);
  std::string out;
  char buf[4096];
  for (;;) {
    ssize_t n = read(pfd[0], buf, sizeof buf);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) break;
    out.append(buf, n);
  }
  close(pfd[0]);
  int st = 0;
  while (waitpid(pid, &st, 0) < 0 && errno == EINTR) {
  }
  subst_status = WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
  last_status = subst_status;
  if (WIFSIGNALED(st) && WTERMSIG(st) == SIGINT && interactive) interrupted = true;
  while (!out.empty() && out.back() == '\n') out.pop_back();
  return out;
}

// --- redirections ------------------------------------------------------------------

bool Shell::apply_redirs(const std::vector<Redir>& redirs, std::vector<std::pair<int, int>>* saved) {
  auto save = [&](int fd) {
    if (!saved) return;
    int copy = fcntl(fd, F_DUPFD_CLOEXEC, 10);
    saved->emplace_back(fd, copy);
  };
  for (const Redir& r : redirs) {
    std::vector<std::string> t;
    try {
      t = expand_word(r.target);
    } catch (const ExpandError& e) {
      err(e.what());
      return false;
    }
    if (t.size() != 1) {
      err(r.target + ": ambiguous redirect");
      return false;
    }
    const std::string& target = t[0];
    const std::string& op = r.op;
    int fd = r.fd >= 0 ? r.fd : (op[0] == '<' ? 0 : 1);
    bool both = op == "&>" || op == "&>>";
    if (op == "<&" || op == ">&") {
      bool digits = !target.empty() && target.find_first_not_of("0123456789") == std::string::npos;
      if (target == "-") {
        save(fd);
        close(fd);
        continue;
      }
      if (digits) {
        int src = std::stoi(target);
        if (fcntl(src, F_GETFD) < 0) {
          err(target + ": Bad file descriptor");
          return false;
        }
        if (src != fd) {
          save(fd);
          dup2(src, fd);
        }
        continue;
      }
      if (op == "<&" || r.fd >= 0) {
        err(target + ": ambiguous redirect");
        return false;
      }
      both = true;  // >&file is &>file
    }
    int flags;
    if (op == "<") {
      flags = O_RDONLY;
    } else if (op == "<>") {
      flags = O_RDWR | O_CREAT;
    } else if (op == ">>" || op == "&>>") {
      flags = O_WRONLY | O_CREAT | O_APPEND;
    } else {
      flags = O_WRONLY | O_CREAT | O_TRUNC;
    }
    int nfd = open(target.c_str(), flags, 0666);
    if (nfd < 0) {
      err(target + ": " + std::strerror(errno));
      return false;
    }
    std::vector<int> fds = both ? std::vector<int>{1, 2} : std::vector<int>{fd};
    bool keep = false;
    for (int f : fds) {
      if (f == nfd) {  // fd was closed, so open() reused it
        keep = true;
        if (saved) saved->emplace_back(f, -1);
        continue;
      }
      save(f);
      dup2(nfd, f);
    }
    if (!keep) close(nfd);
  }
  return true;
}

void Shell::restore_redirs(std::vector<std::pair<int, int>>& saved) {
  for (auto it = saved.rbegin(); it != saved.rend(); ++it) {
    if (it->second >= 0) {
      dup2(it->second, it->first);
      close(it->second);
    } else {
      close(it->first);
    }
  }
  saved.clear();
}

}  // namespace msh
