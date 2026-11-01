// The shell's state and the executor.

#pragma once

#include <signal.h>
#include <sys/types.h>
#include <termios.h>

#include <functional>
#include <map>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>

#include "syntax.hpp"

namespace msh {

// A failed expansion: ${x?}, bad substitution, division by zero, ...
struct ExpandError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

extern volatile sig_atomic_t g_sigint;
extern volatile sig_atomic_t g_sighup;
extern volatile sig_atomic_t g_sigwinch;

struct Var {
  std::string value;
  bool exported = false;
};

struct Proc {
  pid_t pid = 0;
  int status = 0;
  bool done = false;
  bool stopped = false;
};

struct Job {
  int id = 0;
  pid_t pgid = 0;
  std::vector<Proc> procs;
  std::string text;
  bool background = false;
  bool notify = false;  // state changed while in the background: report it
  termios tmodes{};
  bool have_tmodes = false;

  bool all_done() const;
  bool stopped() const;  // nothing running, at least one stopped
  int status() const;    // of the last process, as $? would see it
};

// A command ready to run in a child process: either a parsed node to run
// in the child (pipeline members, compound commands), or an already
// expanded simple command.
struct Launch {
  NodeP node;
  std::vector<std::string> argv;
  std::vector<std::pair<std::string, std::string>> env;
  std::vector<Redir> redirs;
};

class Shell;
using Builtin = int (*)(Shell&, std::vector<std::string>&);
const std::map<std::string, Builtin>& builtins();
std::string signal_name(int sig);  // "INT", "TERM", ...; "" if unknown
int signal_number(const std::string& name);  // accepts INT, SIGINT, 2; -1 if unknown
std::string signal_description(int sig);  // "Terminated", "Killed", ...

class Shell {
 public:
  Shell();

  // --- setup ---------------------------------------------------------------
  void import_environment();
  // Interactive: own process group, the terminal, job control signals.
  void init_interactive();
  bool interactive = false;
  bool job_control = false;
  bool login = false;
  int tty = -1;
  pid_t pgid = 0;
  termios tmodes{};  // the terminal as programs should get it

  // --- running code --------------------------------------------------------
  // Parses and runs `src`. Syntax errors are reported, status 2.
  int run_string(const std::string& src, const std::string& origin = "");
  int run_file(const std::string& path, bool must_exist = true);
  int run(const NodeP& n);
  std::string command_output(const std::string& src);  // $(...)

  // --- variables -----------------------------------------------------------
  std::map<std::string, Var> vars;
  std::optional<std::string> get(const std::string& name) const;
  std::string get_or(const std::string& name, const std::string& def = "") const;
  void set(const std::string& name, const std::string& value, bool exp = false);
  void unset(const std::string& name);
  std::vector<std::string> positional;
  std::string arg0 = "msh";
  int last_status = 0;
  int subst_status = -1;  // status of the last $(...) in the current command
  pid_t top_pid = 0;      // $$: the shell's pid, also in subshells
  pid_t last_bg = 0;
  bool opt_errexit = false;
  bool opt_xtrace = false;
  AliasMap aliases;
  std::map<std::string, NodeP> functions;
  std::vector<std::vector<std::pair<std::string, std::optional<Var>>>> locals;  // per function call

  // --- expansion (expand.cpp) ----------------------------------------------
  std::vector<std::string> expand_words(const std::vector<std::string>& raw);
  std::vector<std::string> expand_word(const std::string& raw);  // split + glob
  std::string expand_string(const std::string& raw);             // no split, no glob
  std::string expand_prompt(const std::string& ps);
  long long arith(const std::string& expr);  // arith.cpp

  // --- control flow --------------------------------------------------------
  int break_levels = 0;
  int continue_levels = 0;
  bool returning = false;
  bool exiting = false;
  int exit_code = 0;
  int loop_depth = 0;
  int func_depth = 0;
  int cond_depth = 0;  // inside if/while conditions, && || lists, `!`: no errexit
  bool interrupted = false;  // Ctrl-C: unwind to the prompt
  bool unwinding() const;
  bool in_child = false;  // a forked subshell; exit with _exit

  // --- jobs (jobs.cpp) -----------------------------------------------------
  std::vector<Job> jobs;
  std::vector<int> job_order;  // most recently used last: %+ is back(), %- before it
  Job* find_job(const std::string& spec, std::string& err);
  Job* job_by_id(int id);
  int add_job(Job job);
  void remove_job(int id);
  void touch_job(int id);
  void update_proc(pid_t pid, int status);
  void reap(bool block);              // collect child status changes
  void notify_jobs();                 // report finished/stopped background jobs
  int wait_foreground(Job& job);      // returns its status; removes it when done
  int continue_job(Job& job, bool foreground);
  std::string job_line(const Job& job, bool with_pids = false) const;
  int launch(std::vector<Launch> cmds, bool background, const std::string& text);
  bool warned_stopped = false;

  // --- I/O -----------------------------------------------------------------
  void err(const std::string& msg) const;  // "msh: msg\n" on stderr
  // Redirections; with `saved`, records how to undo them.
  bool apply_redirs(const std::vector<Redir>& redirs, std::vector<std::pair<int, int>>* saved);
  void restore_redirs(std::vector<std::pair<int, int>>& saved);
  std::string find_command(const std::string& name) const;  // PATH lookup; "" if none

  // --- history (owned by the line editor; builtins reach it through this) --
  std::function<std::vector<std::string>()> history_list;
  std::function<void()> history_clear;
  std::function<void()> on_exit;  // save history etc.
  std::function<void()> resize;

  [[noreturn]] void exit_now(int code);

 private:
  int run_list(const Node& n);
  int run_and_or(const Node& n);
  int run_pipeline(const Node& n);
  int run_simple(const Node& n, bool in_child);
  int run_function(const NodeP& body, std::vector<std::string>& argv);
  int run_compound(const Node& n);
  [[noreturn]] void exec_external(std::vector<std::string>& argv,
                                  const std::vector<std::pair<std::string, std::string>>& env);
  [[noreturn]] void child_run(Launch& l);
  void child_setup(pid_t job_pgid, bool foreground);
  void check_errexit(int status);
};

std::string join(const std::vector<std::string>& v, const std::string& sep);
void write_all(int fd, const std::string& s);

}  // namespace msh
