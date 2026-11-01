//! myos-init: PID 1 for myos.
//!
//! As PID 1 (installed as /sbin/init):
//!   mounts /proc, /sys, /dev, /dev/pts, /dev/shm, /run, /tmp; sets the
//!   hostname from /etc/hostname; starts the services in /etc/myos/services/;
//!   reaps zombies; restarts crashed services; shuts down cleanly on
//!   poweroff / reboot / halt (BusyBox's applets work) or Ctrl-Alt-Del.
//!
//! As a normal command:
//!   initctl status [NAME]           (initctl is a symlink to myos-init)
//!   initctl start|stop|restart NAME
//!   initctl reload                  re-read /etc/myos/services/ (also: kill -HUP 1)
//!   initctl poweroff|reboot|halt
//!   myos-init check [DIR]           validate service files without running them
//!
//! Kernel command line: `myos.emergency` skips the services and gives a root
//! shell on the console.

mod config;
mod ctl;
mod logfwd;
mod mounts;
mod supervisor;
mod sys;
mod toml;

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

pub const SERVICES_DIR: &str = "/etc/myos/services";
pub const CTL_SOCKET: &str = "/run/myos/init.sock";

/// To the console, and (from Phase 3) to logd tagged `init`.
macro_rules! log {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        eprintln!("[init] {msg}");
        $crate::logfwd::own(&msg);
    }};
}
pub(crate) use log;

const USAGE: &str = "\
usage: initctl status [NAME]
       initctl start|stop|restart NAME
       initctl reload
       initctl poweroff|reboot|halt
       myos-init check [DIR]    validate service files (default /etc/myos/services)

myos-init runs as PID 1; started any other way it only acts as a client.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let argv0 = args.first().map(|a| a.rsplit('/').next().unwrap_or(a).to_string()).unwrap_or_default();

    if sys::pid() == 1 {
        return pid1();
    }
    if argv0 == "initctl" {
        return ctl::client(&args[1..], USAGE);
    }
    match args.get(1).map(String::as_str) {
        Some("check") => ctl::check(Path::new(args.get(2).map(String::as_str).unwrap_or(SERVICES_DIR))),
        Some("ctl") => ctl::client(&args[2..], USAGE),
        Some("--version") => {
            println!("myos-init {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => ctl::client(&args[1..], USAGE),
    }
}

fn pid1() -> ExitCode {
    let outcome = std::panic::catch_unwind(boot);
    let why = match outcome {
        Ok(msg) => msg,
        Err(p) => p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "panic".into()),
    };
    // If PID 1 exits the kernel panics. A shell is a better last resort.
    log!("fatal: {why}");
    emergency_shell()
}

/// Normal boot. Returns only on failure.
fn boot() -> String {
    mounts::mount_api_filesystems();
    mounts::attach_console();
    logfwd::activate();
    log!("myos-init {} starting", env!("CARGO_PKG_VERSION"));
    mounts::remount_root_rw();
    mounts::set_hostname();

    if std::fs::read_to_string("/proc/cmdline").unwrap_or_default().split_whitespace().any(|w| w == "myos.emergency") {
        log!("myos.emergency on the kernel command line: skipping services");
        return "emergency mode requested".into();
    }

    let loaded = config::load_dir(Path::new(SERVICES_DIR));
    for e in &loaded.errors {
        log!("skipping {e}");
    }
    for w in &loaded.warnings {
        log!("warning: {w}");
    }
    if loaded.services.is_empty() {
        log!("no services to start");
    }
    supervisor::Supervisor::new(loaded.services).run()
}

fn emergency_shell() -> ExitCode {
    loop {
        log!("starting emergency shell; exit it to start another");
        let child = Command::new("/bin/sh").env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").spawn();
        match child {
            Ok(c) => {
                let pid = c.id() as i32;
                // Reap everything (orphans included) until the shell itself exits.
                while let Some((p, _)) = sys::wait_any() {
                    if p == pid {
                        break;
                    }
                }
            }
            Err(e) => {
                log!("cannot start /bin/sh: {e}");
                // Nothing else we can do; exec'ing anything is better than exiting.
                let err = Command::new("/bin/busybox").arg("sh").exec();
                log!("cannot exec busybox sh either: {err}");
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
        }
    }
}
