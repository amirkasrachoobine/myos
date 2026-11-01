//! The service manager: starts services in dependency order, reaps every
//! child (ours and orphans), restarts what crashes, answers control requests
//! and runs the shutdown sequence.

use crate::config::{self, Kind, Output, Restart, Service};
use crate::{log, logfwd, mounts, sys, CTL_SOCKET, SERVICES_DIR};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::raw::c_int;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
/// A service that stays up this long gets its backoff counter reset.
const STABLE_AFTER: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shutdown {
    Poweroff,
    Reboot,
    Halt,
}

impl Shutdown {
    fn verb(self) -> &'static str {
        match self {
            Shutdown::Poweroff => "powering off",
            Shutdown::Reboot => "rebooting",
            Shutdown::Halt => "halting",
        }
    }
}

#[derive(Debug)]
enum State {
    /// Wanted, waiting for its `after` dependencies.
    Waiting,
    Running { pid: i32, since: Instant },
    Stopping { pid: i32, deadline: Instant, killed: bool },
    /// Crashed; will be started again at `until`.
    Backoff { until: Instant },
    /// Exited and won't be restarted (oneshot finished, or restart policy says no).
    Exited { status: String, ok: bool },
    /// Stopped on request, or disabled.
    Stopped,
}

struct Unit {
    svc: Service,
    state: State,
    /// Should be running (false after `stop`, for disabled services and during shutdown).
    wanted: bool,
    /// Has come up at least once: a simple service was spawned, a oneshot
    /// finished. Services that are `after` this one wait for it.
    up: bool,
    restarts: u32,
    /// `restart` was requested: start again as soon as it has stopped.
    restart_pending: bool,
    /// Its file was removed on reload: forget it once stopped.
    removed: bool,
}

impl Unit {
    fn new(svc: Service) -> Unit {
        let wanted = svc.enabled;
        Unit {
            state: if wanted { State::Waiting } else { State::Stopped },
            svc,
            wanted,
            up: false,
            restarts: 0,
            restart_pending: false,
            removed: false,
        }
    }

    fn pid(&self) -> Option<i32> {
        match self.state {
            State::Running { pid, .. } | State::Stopping { pid, .. } => Some(pid),
            _ => None,
        }
    }

    fn status_line(&self, now: Instant) -> String {
        let state = match &self.state {
            State::Waiting => "waiting".to_string(),
            State::Running { pid, since } => format!("running   pid {pid}, up {}", human(now - *since)),
            State::Stopping { pid, .. } => format!("stopping  pid {pid}"),
            State::Backoff { until } => format!("crashed   restarting in {}", human(until.saturating_duration_since(now))),
            State::Exited { status, ok: true } => format!("done      {status}"),
            State::Exited { status, ok: false } => format!("failed    {status}"),
            State::Stopped if !self.svc.enabled => "disabled".to_string(),
            State::Stopped => "stopped".to_string(),
        };
        let mut line = format!("{:<14} {state}", self.svc.name);
        if self.restarts > 0 {
            line.push_str(&format!("  restarts {}", self.restarts));
        }
        line
    }
}

fn human(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{}s", s / 60, s % 60),
        _ => format!("{}h{}m", s / 3600, s % 3600 / 60),
    }
}

fn describe(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(c), _) => format!("exit code {c}"),
        (_, Some(s)) => format!("killed by signal {s}"),
        _ => format!("{status}"),
    }
}

fn console(write_only: bool) -> Stdio {
    let f = OpenOptions::new().read(!write_only).write(true).custom_flags(sys::O_NOCTTY).open("/dev/console");
    f.map(Stdio::from).unwrap_or_else(|_| Stdio::null())
}

/// The read end of a service's stdout/stderr pipe. It's kept until EOF,
/// which comes once the service and everything it started have exited.
struct OutputPipe {
    tag: String,
    pid: i32,
    reader: io::PipeReader,
    lines: logfwd::LineBuffer,
}

impl OutputPipe {
    /// Reads what's there (the fd is readable). Returns false at EOF.
    fn read(&mut self) -> bool {
        let mut buf = [0u8; 4096];
        let (tag, pid) = (&self.tag, self.pid);
        match self.reader.read(&mut buf) {
            Ok(0) | Err(_) => {
                self.lines.finish(|l| logfwd::line(tag, pid, l));
                false
            }
            Ok(n) => {
                self.lines.push(&buf[..n], |l| logfwd::line(tag, pid, l));
                true
            }
        }
    }
}

fn spawn(svc: &Service) -> io::Result<(i32, Option<io::PipeReader>)> {
    let mut cmd = Command::new(&svc.exec[0]);
    cmd.args(&svc.exec[1..])
        .env_clear()
        .env("PATH", DEFAULT_PATH)
        .env("HOME", "/")
        .env("TERM", std::env::var("TERM").unwrap_or_else(|_| "linux".into()))
        .envs(svc.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&svc.cwd);
    let mut reader = None;
    if svc.console {
        cmd.stdin(console(false)).stdout(console(true)).stderr(console(true));
    } else if svc.output == Output::Log && logfwd::enabled() {
        // Both ends are close-on-exec; Command dup2s the write end onto 1 and 2.
        let (r, w) = io::pipe()?;
        let w2 = w.try_clone()?;
        cmd.stdin(Stdio::null()).stdout(w).stderr(w2);
        reader = Some(r);
    } else {
        cmd.stdin(Stdio::null()).stdout(console(true)).stderr(console(true));
    }
    // SAFETY: child_setup only makes async-signal-safe libc calls.
    unsafe {
        cmd.pre_exec(sys::child_setup);
    }
    // Dropping the Child doesn't wait for it; we reap every child ourselves.
    let pid = cmd.spawn()?.id() as i32;
    // Our copies of the write ends live in `cmd`; close them so the pipe
    // reports EOF once the service's side is closed.
    drop(cmd);
    Ok((pid, reader))
}

pub struct Supervisor {
    units: BTreeMap<String, Unit>,
    shutdown: Option<(Shutdown, Instant)>,
    outputs: Vec<OutputPipe>,
}

impl Supervisor {
    pub fn new(services: Vec<Service>) -> Supervisor {
        Supervisor {
            units: services.into_iter().map(|s| (s.name.clone(), Unit::new(s))).collect(),
            shutdown: None,
            outputs: Vec::new(),
        }
    }

    /// The main loop. Returns only if something is badly wrong.
    pub fn run(mut self) -> String {
        let sigfd = match sys::block_signals_and_open_signalfd(&[
            sys::SIGCHLD,
            sys::SIGHUP,
            sys::SIGINT,
            sys::SIGTERM,
            sys::SIGUSR1,
            sys::SIGUSR2,
            sys::SIGPWR,
        ]) {
            Ok(fd) => fd,
            Err(e) => return format!("cannot set up signal handling: {e}"),
        };
        // Ctrl-Alt-Del now sends us SIGINT instead of rebooting instantly.
        let _ = sys::do_reboot(sys::RB_DISABLE_CAD);
        let listener = open_control_socket();

        loop {
            self.start_ready();
            if let Some((kind, deadline)) = self.shutdown {
                if self.units.values().all(|u| u.pid().is_none()) || Instant::now() >= deadline {
                    return finish(kind);
                }
            }

            let mut fds = vec![sigfd];
            if let Some(l) = &listener {
                fds.push(l.as_raw_fd());
            }
            let first_output = fds.len();
            fds.extend(self.outputs.iter().map(|o| o.reader.as_raw_fd()));
            let ready = sys::poll_readable(&fds, self.next_timeout_ms());

            // Output first: a service's last words before it exits belong
            // before init's "exited" line.
            let mut i = 0;
            self.outputs.retain_mut(|o| {
                let keep = !ready[first_output + i] || o.read();
                i += 1;
                keep
            });
            if ready[0] {
                for sig in sys::read_signals(sigfd) {
                    self.on_signal(sig);
                }
            }
            if ready.get(1) == Some(&true) {
                if let Some(l) = &listener {
                    self.accept_control(l);
                }
            }
            self.on_timers(Instant::now());
            logfwd::flush();
        }
    }

    fn on_signal(&mut self, sig: c_int) {
        match sig {
            sys::SIGCHLD => self.reap(),
            sys::SIGHUP => {
                let msg = self.reload();
                log!("reload: {msg}");
            }
            // BusyBox's halt / poweroff / reboot applets signal PID 1 like this.
            sys::SIGUSR1 => self.begin_shutdown(Shutdown::Halt),
            sys::SIGUSR2 | sys::SIGPWR => self.begin_shutdown(Shutdown::Poweroff),
            sys::SIGTERM | sys::SIGINT => self.begin_shutdown(Shutdown::Reboot),
            _ => {}
        }
    }

    fn reap(&mut self) {
        while let Some((pid, raw)) = sys::reap_one() {
            let status = ExitStatus::from_raw(raw);
            let Some(name) = self.units.iter().find(|(_, u)| u.pid() == Some(pid)).map(|(n, _)| n.clone()) else {
                continue; // an orphan reparented to us: reaping it is all we need to do
            };
            self.on_exit(&name, status);
        }
    }

    fn on_exit(&mut self, name: &str, status: ExitStatus) {
        let shutting_down = self.shutdown.is_some();
        let unit = self.units.get_mut(name).unwrap();
        let now = Instant::now();
        let what = describe(status);
        match unit.state {
            State::Stopping { .. } => {
                unit.state = State::Stopped;
                if unit.removed {
                    self.units.remove(name);
                    log!("{name} stopped and removed");
                    return;
                }
                if unit.restart_pending && !shutting_down {
                    unit.restart_pending = false;
                    unit.wanted = true;
                    unit.state = State::Waiting;
                } else {
                    log!("{name} stopped");
                }
            }
            State::Running { since, .. } => {
                let ok = status.success();
                if unit.svc.kind == Kind::Oneshot {
                    unit.up = true;
                    unit.state = State::Exited { status: what.clone(), ok };
                    if !ok {
                        log!("{name} failed: {what}");
                    }
                    return;
                }
                let restart = unit.wanted
                    && !shutting_down
                    && match unit.svc.restart {
                        Restart::Always => true,
                        Restart::OnFailure => !ok,
                        Restart::Never => false,
                    };
                if !restart {
                    log!("{name} exited: {what}");
                    unit.state = State::Exited { status: what, ok };
                    return;
                }
                if now - since >= STABLE_AFTER {
                    unit.restarts = 0;
                }
                let delay = backoff(unit.restarts);
                unit.restarts += 1;
                log!("{name} exited ({what}); restarting in {}", human(delay));
                unit.state = State::Backoff { until: now + delay };
            }
            _ => {}
        }
    }

    /// Start everything that's wanted and whose dependencies are up. Loops
    /// because starting a simple service can unblock the next one.
    fn start_ready(&mut self) {
        if self.shutdown.is_some() {
            return;
        }
        loop {
            let ready: Vec<String> = self
                .units
                .iter()
                .filter(|(_, u)| u.wanted && matches!(u.state, State::Waiting))
                .filter(|(_, u)| u.svc.after.iter().all(|d| self.dependency_satisfied(d)))
                .map(|(n, _)| n.clone())
                .collect();
            if ready.is_empty() {
                return;
            }
            for name in ready {
                self.start_now(&name);
            }
        }
    }

    /// `after` is ordering only: we wait for a dependency to come up, but if
    /// it isn't going to (stopped, disabled, removed), we don't wait forever.
    fn dependency_satisfied(&self, dep: &str) -> bool {
        match self.units.get(dep) {
            None => true,
            Some(u) => u.up || !u.wanted || matches!(u.state, State::Exited { .. }),
        }
    }

    fn start_now(&mut self, name: &str) {
        let result = match spawn(&self.units[name].svc) {
            Ok((pid, reader)) => {
                if let Some(reader) = reader {
                    let lines = logfwd::LineBuffer::new();
                    self.outputs.push(OutputPipe { tag: name.to_string(), pid, reader, lines });
                }
                Ok(pid)
            }
            Err(e) => Err(e),
        };
        let unit = self.units.get_mut(name).unwrap();
        match result {
            Ok(pid) => {
                log!("started {name} (pid {pid})");
                unit.state = State::Running { pid, since: Instant::now() };
                if unit.svc.kind == Kind::Simple {
                    unit.up = true;
                }
            }
            Err(e) => {
                let what = format!("cannot run {}: {e}", unit.svc.exec[0]);
                log!("{name}: {what}");
                unit.up = true; // don't block dependents on a service that can't start
                if unit.svc.kind == Kind::Simple && unit.svc.restart != Restart::Never {
                    let delay = backoff(unit.restarts);
                    unit.restarts += 1;
                    unit.state = State::Backoff { until: Instant::now() + delay };
                } else {
                    unit.state = State::Exited { status: what, ok: false };
                }
            }
        }
    }

    fn stop(&mut self, name: &str) {
        let Some(unit) = self.units.get_mut(name) else { return };
        unit.wanted = false;
        match unit.state {
            State::Running { pid, .. } => {
                sys::signal_service(pid, unit.svc.stop_signal);
                unit.state = State::Stopping { pid, deadline: Instant::now() + unit.svc.stop_timeout, killed: false };
            }
            State::Stopping { .. } => {}
            _ => {
                unit.state = State::Stopped;
                if unit.removed {
                    self.units.remove(name);
                }
            }
        }
    }

    fn on_timers(&mut self, now: Instant) {
        for (name, unit) in self.units.iter_mut() {
            match unit.state {
                State::Backoff { until } if now >= until => unit.state = State::Waiting,
                State::Stopping { pid, deadline, killed: false } if now >= deadline => {
                    log!("{name} didn't stop within {}s; killing it", unit.svc.stop_timeout.as_secs());
                    sys::signal_service(pid, sys::SIGKILL);
                    unit.state = State::Stopping { pid, deadline, killed: true };
                }
                _ => {}
            }
        }
    }

    fn next_timeout_ms(&self) -> i32 {
        let now = Instant::now();
        let mut next: Option<Instant> = self.shutdown.map(|(_, d)| d);
        if logfwd::pending() {
            // Lines are waiting for logd; try again soon.
            next = Some(now + Duration::from_millis(100));
        }
        for u in self.units.values() {
            let t = match u.state {
                State::Backoff { until } => until,
                State::Stopping { deadline, killed: false, .. } => deadline,
                _ => continue,
            };
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        match next {
            None => -1,
            Some(t) => (t.saturating_duration_since(now).as_millis() as i32).saturating_add(1),
        }
    }

    fn begin_shutdown(&mut self, kind: Shutdown) {
        if self.shutdown.is_some() {
            return;
        }
        log!("{}: stopping services", kind.verb());
        let longest = self.units.values().map(|u| u.svc.stop_timeout).max().unwrap_or_default();
        self.shutdown = Some((kind, Instant::now() + longest + Duration::from_secs(3)));
        let names: Vec<String> = self.units.keys().cloned().collect();
        for name in names {
            self.stop(&name);
        }
    }

    fn reload(&mut self) -> String {
        if self.shutdown.is_some() {
            return "error: shutting down".into();
        }
        let loaded = config::load_dir(Path::new(SERVICES_DIR));
        for e in loaded.errors.iter().chain(&loaded.warnings) {
            log!("{e}");
        }
        let mut added = Vec::new();
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        let new_names: Vec<String> = loaded.services.iter().map(|s| s.name.clone()).collect();
        let old_names: Vec<String> = self.units.keys().cloned().collect();
        for name in old_names {
            if !new_names.contains(&name) {
                self.units.get_mut(&name).unwrap().removed = true;
                self.stop(&name);
                removed.push(name);
            }
        }
        for svc in loaded.services {
            let name = svc.name.clone();
            match self.units.get_mut(&name) {
                None => {
                    self.units.insert(name.clone(), Unit::new(svc));
                    added.push(name);
                }
                Some(u) if u.svc != svc => {
                    let now_disabled = u.svc.enabled && !svc.enabled;
                    let now_enabled = !u.svc.enabled && svc.enabled;
                    u.svc = svc;
                    if now_disabled {
                        self.stop(&name);
                    } else if now_enabled {
                        u.wanted = true;
                        u.state = State::Waiting;
                    }
                    changed.push(name);
                }
                Some(_) => {}
            }
        }
        let mut parts = Vec::new();
        for (label, list) in [("added", &added), ("changed", &changed), ("removed", &removed)] {
            if !list.is_empty() {
                parts.push(format!("{label}: {}", list.join(", ")));
            }
        }
        if parts.is_empty() {
            parts.push("no changes".into());
        }
        if !changed.is_empty() {
            parts.push("restart changed services to apply".into());
        }
        if !loaded.errors.is_empty() {
            parts.push(format!("{} file(s) skipped, see console", loaded.errors.len()));
        }
        parts.join("; ")
    }

    // ---- control socket ----------------------------------------------------

    fn accept_control(&mut self, listener: &UnixListener) {
        while let Ok((stream, _)) = listener.accept() {
            let _ = self.serve(stream);
        }
    }

    fn serve(&mut self, stream: UnixStream) -> io::Result<()> {
        stream.set_nonblocking(false)?;
        // A stuck client must never stall PID 1.
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).take(4096).read_line(&mut line)?;
        let reply = self.command(line.trim());
        let mut stream = stream;
        stream.write_all(reply.as_bytes())?;
        if !reply.ends_with('\n') {
            stream.write_all(b"\n")?;
        }
        Ok(())
    }

    fn command(&mut self, line: &str) -> String {
        let words: Vec<&str> = line.split_whitespace().collect();
        let now = Instant::now();
        match words.as_slice() {
            ["status"] => {
                let mut out = String::new();
                for u in self.units.values() {
                    out.push_str(&u.status_line(now));
                    out.push('\n');
                }
                if out.is_empty() {
                    out = format!("no services in {SERVICES_DIR}\n");
                }
                out
            }
            ["status", name] => match self.units.get(*name) {
                Some(u) => u.status_line(now),
                None => format!("error: no service named {name}"),
            },
            ["start" | "stop" | "restart", _] if self.shutdown.is_some() => "error: shutting down".into(),
            ["start", name] => match self.units.get_mut(*name) {
                None => format!("error: no service named {name}"),
                Some(u) => match u.state {
                    State::Running { .. } => format!("{name} is already running"),
                    State::Stopping { .. } => {
                        u.restart_pending = true;
                        format!("{name} is stopping; it will start again")
                    }
                    _ => {
                        u.wanted = true;
                        u.restarts = 0;
                        u.state = State::Waiting;
                        self.start_ready();
                        self.units[*name].status_line(now)
                    }
                },
            },
            ["stop", name] => {
                if !self.units.contains_key(*name) {
                    return format!("error: no service named {name}");
                }
                self.stop(name);
                self.units.get(*name).map(|u| u.status_line(now)).unwrap_or_default()
            }
            ["restart", name] => match self.units.get_mut(*name) {
                None => format!("error: no service named {name}"),
                Some(u) => {
                    u.restarts = 0;
                    if u.pid().is_some() {
                        u.restart_pending = true;
                        self.stop(name);
                        format!("restarting {name}")
                    } else {
                        u.wanted = true;
                        u.state = State::Waiting;
                        self.start_ready();
                        self.units[*name].status_line(now)
                    }
                }
            },
            ["reload"] => self.reload(),
            ["poweroff"] => self.shutdown_reply(Shutdown::Poweroff),
            ["reboot"] => self.shutdown_reply(Shutdown::Reboot),
            ["halt"] => self.shutdown_reply(Shutdown::Halt),
            _ => format!("error: unknown command `{line}`; try: status [NAME], start|stop|restart NAME, reload, poweroff, reboot, halt"),
        }
    }

    fn shutdown_reply(&mut self, kind: Shutdown) -> String {
        self.begin_shutdown(kind);
        kind.verb().to_string()
    }
}

/// 1s, 2s, 4s, 8s, 16s, 30s, 30s, ...
fn backoff(restarts: u32) -> Duration {
    Duration::from_secs(1u64 << restarts.min(5)).min(MAX_BACKOFF)
}

fn open_control_socket() -> Option<UnixListener> {
    let _ = std::fs::remove_file(CTL_SOCKET);
    match UnixListener::bind(CTL_SOCKET) {
        Ok(l) => {
            let _ = std::fs::set_permissions(CTL_SOCKET, std::fs::Permissions::from_mode(0o600));
            let _ = l.set_nonblocking(true);
            Some(l)
        }
        Err(e) => {
            log!("control socket {CTL_SOCKET} unavailable: {e}");
            None
        }
    }
}

/// Services are down: clean up stragglers, sync, and hand over to the kernel.
fn finish(kind: Shutdown) -> String {
    log!("sending SIGTERM to remaining processes");
    let _ = sys::send_signal(-1, sys::SIGTERM);
    reap_until_empty(Duration::from_secs(2));
    let _ = sys::send_signal(-1, sys::SIGKILL);
    reap_until_empty(Duration::from_secs(1));
    log!("syncing filesystems");
    sys::sync_all();
    mounts::remount_root_ro();
    sys::sync_all();
    log!("{}", kind.verb());
    let cmd = match kind {
        Shutdown::Poweroff => sys::RB_POWER_OFF,
        Shutdown::Reboot => sys::RB_AUTOBOOT,
        Shutdown::Halt => sys::RB_HALT_SYSTEM,
    };
    match sys::do_reboot(cmd) {
        Ok(()) => "reboot() returned".into(),
        Err(e) => format!("reboot() failed: {e}"),
    }
}

fn reap_until_empty(limit: Duration) {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        while sys::reap_one().is_some() {}
        // ECHILD from a probe means nothing is left.
        if std::fs::read_dir("/proc")
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<i32>().ok()))
                    .all(|pid| pid == 1 || is_kernel_thread(pid))
            })
            .unwrap_or(true)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kernel threads have an empty cmdline and are immune to kill(-1).
fn is_kernel_thread(pid: i32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline")).map(|c| c.is_empty()).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let secs: Vec<u64> = (0..8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn human_durations() {
        assert_eq!(human(Duration::from_secs(5)), "5s");
        assert_eq!(human(Duration::from_secs(125)), "2m5s");
        assert_eq!(human(Duration::from_secs(7322)), "2h2m");
    }
}
