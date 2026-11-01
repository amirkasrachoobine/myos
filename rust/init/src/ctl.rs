//! The client side: `initctl ...` talks to PID 1 over a Unix socket, and
//! `myos-init check` validates service files on any machine.
//!
//! The socket is a stopgap until the Phase 2 IPC bus; the protocol is one
//! request line in, a text reply out, then the connection closes. Replies
//! starting with "error:" make initctl exit with status 1.

use crate::{config, CTL_SOCKET};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;

pub fn client(args: &[String], usage: &str) -> ExitCode {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        eprintln!("{usage}");
        return ExitCode::from(2);
    }
    let request = args.join(" ");
    let mut stream = match UnixStream::connect(CTL_SOCKET) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("initctl: can't reach init at {CTL_SOCKET}: {e}");
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("initctl: (it's root-only)");
            }
            return ExitCode::FAILURE;
        }
    };
    let mut reply = String::new();
    let result = stream
        .write_all(format!("{request}\n").as_bytes())
        .and_then(|_| stream.shutdown(Shutdown::Write))
        .and_then(|_| stream.read_to_string(&mut reply));
    if let Err(e) = result {
        eprintln!("initctl: {e}");
        return ExitCode::FAILURE;
    }
    print!("{reply}");
    if reply.starts_with("error:") { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

pub fn check(dir: &Path) -> ExitCode {
    let loaded = config::load_dir(dir);
    for s in &loaded.services {
        let kind = match s.kind {
            config::Kind::Simple => "simple",
            config::Kind::Oneshot => "oneshot",
        };
        let mut line = format!("ok       {:<14} {kind:<7} {}", s.name, s.exec.join(" "));
        if !s.after.is_empty() {
            line.push_str(&format!("  (after {})", s.after.join(", ")));
        }
        if !s.enabled {
            line.push_str("  [disabled]");
        }
        println!("{line}");
    }
    for w in &loaded.warnings {
        println!("warning  {w}");
    }
    for e in &loaded.errors {
        println!("error    {e}");
    }
    if loaded.errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
