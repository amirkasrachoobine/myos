//! busd: the myos IPC bus broker.
//!
//! Listens on /run/myos/bus.sock (or --socket / $MYOS_BUS_SOCKET), routes
//! calls between clients and the services that registered their names, and
//! answers the built-in "bus" service (Ping, ListServices) itself. The
//! protocol is in proto/myos/bus/v1/wire.proto.

mod broker;
mod policy;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;

macro_rules! log {
    ($($arg:tt)*) => { eprintln!("[busd] {}", format_args!($($arg)*)) };
}
pub(crate) use log;

const USAGE: &str = "\
usage: busd [-v] [--socket PATH] [--policy-dir DIR]

  --socket PATH     listen here (default $MYOS_BUS_SOCKET, else /run/myos/bus.sock)
  --policy-dir DIR  app policies, one file per app uid (default
                    $MYOS_BUS_POLICY_DIR, else /etc/myos/bus/policy.d)
  -v              log every connection
  --version";

fn main() -> ExitCode {
    let mut socket: Option<PathBuf> = None;
    let mut policy_dir: PathBuf = match std::env::var_os("MYOS_BUS_POLICY_DIR") {
        Some(d) if !d.is_empty() => d.into(),
        _ => myos_bus::DEFAULT_POLICY_DIR.into(),
    };
    let mut verbose = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => match args.next() {
                Some(p) => socket = Some(p.into()),
                None => return usage(),
            },
            "--policy-dir" => match args.next() {
                Some(p) => policy_dir = p.into(),
                None => return usage(),
            },
            "-v" | "--verbose" => verbose = true,
            "--version" => {
                println!("busd {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ => return usage(),
        }
    }
    let path = socket.unwrap_or_else(myos_bus::socket_path);

    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log!("can't create {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    }
    // A socket file left over from a previous run blocks bind(). Remove it,
    // unless a live broker is still answering on it.
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            log!("another broker is already listening on {}", path.display());
            return ExitCode::FAILURE;
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log!("can't listen on {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    // Anyone may connect; what they may do is checked per call, using the
    // credentials the kernel reports for the socket.
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)) {
        log!("warning: can't chmod {}: {e}", path.display());
    }
    log!("busd {} listening on {}", env!("CARGO_PKG_VERSION"), path.display());

    let policies = policy::Policies::new(policy_dir);
    log!("app policies (uids {}-{}) from {}", policy::APP_UID_MIN, policy::APP_UID_MAX, policies.dir().display());
    let err = broker::Broker::new(verbose, policies).serve(listener);
    log!("accept failed: {err}");
    ExitCode::FAILURE
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
