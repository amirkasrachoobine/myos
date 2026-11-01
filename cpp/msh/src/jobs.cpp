// Job control: the job table, waiting for the foreground job, reporting
// background jobs, and fg/bg.

#include <errno.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cstring>

#include "shell.hpp"

namespace msh {

namespace {

struct SigName {
  int num;
  const char* name;
  const char* desc;
};

const SigName kSignals[] = {
    {SIGHUP, "HUP", "Hangup"},
    {SIGINT, "INT", "Interrupt"},
    {SIGQUIT, "QUIT", "Quit"},
    {SIGILL, "ILL", "Illegal instruction"},
    {SIGTRAP, "TRAP", "Trace/breakpoint trap"},
    {SIGABRT, "ABRT", "Aborted"},
    {SIGBUS, "BUS", "Bus error"},
    {SIGFPE, "FPE", "Floating point exception"},
    {SIGKILL, "KILL", "Killed"},
    {SIGUSR1, "USR1", "User defined signal 1"},
    {SIGSEGV, "SEGV", "Segmentation fault"},
    {SIGUSR2, "USR2", "User defined signal 2"},
    {SIGPIPE, "PIPE", "Broken pipe"},
    {SIGALRM, "ALRM", "Alarm clock"},
    {SIGTERM, "TERM", "Terminated"},
    {SIGCHLD, "CHLD", "Child exited"},
    {SIGCONT, "CONT", "Continued"},
    {SIGSTOP, "STOP", "Stopped (signal)"},
    {SIGTSTP, "TSTP", "Stopped"},
    {SIGTTIN, "TTIN", "Stopped (tty input)"},
    {SIGTTOU, "TTOU", "Stopped (tty output)"},
    {SIGURG, "URG", "Urgent I/O condition"},
    {SIGXCPU, "XCPU", "CPU time limit exceeded"},
    {SIGXFSZ, "XFSZ", "File size limit exceeded"},
    {SIGVTALRM, "VTALRM", "Virtual timer expired"},
    {SIGPROF, "PROF", "Profiling timer expired"},
    {SIGWINCH, "WINCH", "Window changed"},
    {SIGIO, "IO", "I/O possible"},
    {SIGPWR, "PWR", "Power failure"},
    {SIGSYS, "SYS", "Bad system call"},
};

std::string pad(std::string s, size_t w) {
  if (s.size() < w) s.append(w - s.size(), ' ');
  return s;
}

}  // namespace

std::string signal_name(int sig) {
  for (const SigName& s : kSignals) {
    if (s.num == sig) return s.name;
  }
  return "";
}

int signal_number(const std::string& name) {
  if (!name.empty() && name.find_first_not_of("0123456789") == std::string::npos) {
    int n = std::stoi(name);
    return n < 65 ? n : -1;
  }
  std::string n = name;
  for (char& c : n) c = static_cast<char>(std::toupper(static_cast<unsigned char>(c)));
  if (n.compare(0, 3, "SIG") == 0) n = n.substr(3);
  for (const SigName& s : kSignals) {
    if (n == s.name) return s.num;
  }
  return -1;
}

std::string signal_description(int sig) {
  for (const SigName& s : kSignals) {
    if (s.num == sig) return s.desc;
  }
  return "Signal " + std::to_string(sig);
}

bool Job::all_done() const {
  for (const Proc& p : procs) {
    if (!p.done) return false;
  }
  return true;
}

bool Job::stopped() const {
  bool any = false;
  for (const Proc& p : procs) {
    if (!p.done && !p.stopped) return false;
    any = any || p.stopped;
  }
  return any;
}

int Job::status() const {
  if (procs.empty()) return 0;
  const Proc& p = procs.back();
  if (p.stopped) return 128 + WSTOPSIG(p.status);
  if (WIFEXITED(p.status)) return WEXITSTATUS(p.status);
  if (WIFSIGNALED(p.status)) return 128 + WTERMSIG(p.status);
  return 0;
}

Job* Shell::job_by_id(int id) {
  for (Job& j : jobs) {
    if (j.id == id) return &j;
  }
  return nullptr;
}

int Shell::add_job(Job job) {
  int id = 1;
  for (const Job& j : jobs) id = std::max(id, j.id + 1);
  job.id = id;
  jobs.push_back(std::move(job));
  job_order.push_back(id);
  return id;
}

void Shell::remove_job(int id) {
  for (size_t i = 0; i < jobs.size(); ++i) {
    if (jobs[i].id == id) {
      jobs.erase(jobs.begin() + i);
      break;
    }
  }
  for (size_t i = 0; i < job_order.size(); ++i) {
    if (job_order[i] == id) {
      job_order.erase(job_order.begin() + i);
      break;
    }
  }
}

void Shell::touch_job(int id) {
  for (size_t i = 0; i < job_order.size(); ++i) {
    if (job_order[i] == id) {
      job_order.erase(job_order.begin() + i);
      break;
    }
  }
  job_order.push_back(id);
}

Job* Shell::find_job(const std::string& spec, std::string& error) {
  std::string s = spec;
  if (!s.empty() && s[0] == '%') s = s.substr(1);
  if (s.empty() || s == "%" || s == "+") {
    if (job_order.empty()) {
      error = "current: no such job";
      return nullptr;
    }
    return job_by_id(job_order.back());
  }
  if (s == "-") {
    if (job_order.size() < 2) {
      error = "previous: no such job";
      return nullptr;
    }
    return job_by_id(job_order[job_order.size() - 2]);
  }
  if (s.find_first_not_of("0123456789") == std::string::npos) {
    Job* j = job_by_id(std::stoi(s));
    if (!j) error = spec + ": no such job";
    return j;
  }
  Job* hit = nullptr;
  bool contains = s[0] == '?';
  std::string needle = contains ? s.substr(1) : s;
  for (Job& j : jobs) {
    bool m = contains ? j.text.find(needle) != std::string::npos : j.text.compare(0, needle.size(), needle) == 0;
    if (!m) continue;
    if (hit) {
      error = spec + ": ambiguous job spec";
      return nullptr;
    }
    hit = &j;
  }
  if (!hit) error = spec + ": no such job";
  return hit;
}

void Shell::update_proc(pid_t pid, int status) {
  for (Job& j : jobs) {
    for (Proc& p : j.procs) {
      if (p.pid != pid) continue;
      if (WIFSTOPPED(status)) {
        p.stopped = true;
        p.status = status;
      } else if (WIFCONTINUED(status)) {
        p.stopped = false;
      } else {
        p.done = true;
        p.stopped = false;
        p.status = status;
      }
      if (j.background && (j.all_done() || j.stopped())) j.notify = true;
      return;
    }
  }
}

void Shell::reap(bool block) {
  for (;;) {
    int st;
    pid_t p = waitpid(-1, &st, (block ? 0 : WNOHANG) | WUNTRACED | WCONTINUED);
    if (p < 0 && errno == EINTR) {
      if (block) return;
      continue;
    }
    if (p <= 0) return;
    update_proc(p, st);
    if (block) return;
  }
}

std::string Shell::job_line(const Job& j, bool with_pids) const {
  char mark = ' ';
  if (!job_order.empty() && job_order.back() == j.id) mark = '+';
  if (job_order.size() > 1 && job_order[job_order.size() - 2] == j.id) mark = '-';
  std::string state;
  bool running = false;
  if (j.all_done()) {
    const Proc& p = j.procs.back();
    if (WIFSIGNALED(p.status)) {
      state = signal_description(WTERMSIG(p.status));
    } else if (WEXITSTATUS(p.status) != 0) {
      state = "Exit " + std::to_string(WEXITSTATUS(p.status));
    } else {
      state = "Done";
    }
  } else if (j.stopped()) {
    int sig = SIGTSTP;
    for (const Proc& p : j.procs) {
      if (p.stopped) sig = WSTOPSIG(p.status);
    }
    state = signal_description(sig);
  } else {
    state = "Running";
    running = true;
  }
  std::string head = "[" + std::to_string(j.id) + "]" + mark + "  ";
  if (with_pids) head += std::to_string(j.procs[0].pid) + " ";
  return head + pad(state, 24) + j.text + (running ? " &" : "");
}

void Shell::notify_jobs() {
  reap(false);
  std::vector<int> done;
  for (Job& j : jobs) {
    if (!j.notify) continue;
    j.notify = false;
    if (interactive) write_all(2, job_line(j) + "\n");
    if (j.all_done()) done.push_back(j.id);
  }
  for (int id : done) remove_job(id);
}

int Shell::wait_foreground(Job& job) {
  int id = job.id;
  if (job_control) tcsetpgrp(tty, job.pgid);
  while (!job.all_done() && !job.stopped()) {
    int st;
    pid_t p = waitpid(-1, &st, WUNTRACED);
    if (p < 0) {
      if (errno == EINTR) continue;
      for (Proc& pr : job.procs) pr.done = true;  // ECHILD: nothing left to wait for
      break;
    }
    update_proc(p, st);
  }
  if (job_control) {
    tcsetpgrp(tty, pgid);
    if (job.stopped()) {
      job.have_tmodes = tcgetattr(tty, &job.tmodes) == 0;
    }
    tcsetattr(tty, TCSADRAIN, &tmodes);
  }
  int st = job.status();
  if (job.stopped()) {
    job.background = true;
    job.notify = false;
    touch_job(id);
    write_all(2, "\n" + job_line(job) + "\n");
    return st;
  }
  const Proc& last = job.procs.back();
  if (WIFSIGNALED(last.status)) {
    int sig = WTERMSIG(last.status);
    if (sig == SIGINT) {
      interrupted = true;
      write_all(2, "\n");
    } else if (sig != SIGPIPE) {
      write_all(2, signal_description(sig) + (WCOREDUMP(last.status) ? " (core dumped)" : "") + "\n");
    }
  }
  remove_job(id);
  return st;
}

int Shell::continue_job(Job& job, bool foreground) {
  for (Proc& p : job.procs) {
    if (!p.done) p.stopped = false;
  }
  touch_job(job.id);
  if (foreground) {
    job.background = false;
    job.notify = false;
    write_all(2, job.text + "\n");
    if (job.have_tmodes) tcsetattr(tty, TCSADRAIN, &job.tmodes);
    tcsetpgrp(tty, job.pgid);
    if (kill(-job.pgid, SIGCONT) < 0) err("fg: " + std::string(std::strerror(errno)));
    return wait_foreground(job);
  }
  job.background = true;
  if (kill(-job.pgid, SIGCONT) < 0) err("bg: " + std::string(std::strerror(errno)));
  write_all(2, "[" + std::to_string(job.id) + "]+ " + job.text + " &\n");
  return 0;
}

}  // namespace msh
