//! logd: the myos log daemon (Phase 3). Replaces BusyBox syslogd and klogd.
//!
//! Collects from:
//!   /dev/log    syslog datagrams: BusyBox logger, anything using syslog(3),
//!               and init, which sends every service's stdout/stderr here
//!   /dev/kmsg   kernel messages, from boot onwards
//!   the bus     log.Write calls
//! and keeps the newest entries in memory for log.Query, and all of them in
//! /var/log/messages (rotated to messages.1 at 1 MiB).
//!
//! Settings, from the environment (for tests, and for the service file):
//!   MYOS_LOGD_SOCKET    /dev/log
//!   MYOS_LOGD_KMSG      /dev/kmsg              ("none": don't read it)
//!   MYOS_LOGD_FILE      /var/log/messages      ("none": memory only)
//!   MYOS_LOGD_FILE_MAX  1048576 (bytes)
//!   MYOS_LOGD_MEMORY    5000 (entries)
//!   MYOS_LOGD_STATE     /run/myos/logd.kmsg-seq   kernel messages already logged

mod parse;
mod store;

use myos_bus::{reply, Request, Status};
use myos_services::proto::log::{Entry, QueryRequest, StatsRequest, WriteReply, WriteRequest};
use myos_services::{error, info, now_us, warning};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::raw::c_void;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use store::{LogFile, Store};

type Shared = Arc<Mutex<Store>>;

fn setting(name: &str, default: &str) -> Option<PathBuf> {
    match std::env::var(name) {
        Ok(v) if v == "none" || v.is_empty() => None,
        Ok(v) => Some(v.into()),
        Err(_) => Some(default.into()),
    }
}

fn number(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() -> ExitCode {
    let socket = setting("MYOS_LOGD_SOCKET", "/dev/log").unwrap_or_else(|| "/dev/log".into());
    let kmsg = setting("MYOS_LOGD_KMSG", "/dev/kmsg");
    let file = setting("MYOS_LOGD_FILE", "/var/log/messages");
    let state = setting("MYOS_LOGD_STATE", "/run/myos/logd.kmsg-seq");

    let file = file.map(|p| LogFile::open(p, number("MYOS_LOGD_FILE_MAX", 1 << 20)));
    let store: Shared = Arc::new(Mutex::new(Store::new(number("MYOS_LOGD_MEMORY", 5000) as usize, file)));

    // The kernel's default queue for a datagram socket is only 10 messages,
    // which a burst at boot overflows (init then holds the rest back and
    // retries). Raise it, as systemd does; this only affects sockets created
    // afterwards, so do it before binding. Only as the real system logger:
    // a test run shouldn't change the host's settings.
    if socket == Path::new("/dev/log") {
        raise_dgram_queue(512);
    }

    let sock = match bind(&socket) {
        Ok(s) => s,
        Err(e) => {
            error!("can't listen on {}: {e}", socket.display());
            return ExitCode::FAILURE;
        }
    };

    if let Some(path) = kmsg {
        let store = store.clone();
        std::thread::Builder::new()
            .name("kmsg".into())
            .spawn(move || read_kmsg(&path, state.as_deref(), &store))
            .expect("spawn kmsg thread");
    }
    {
        let store = store.clone();
        std::thread::Builder::new()
            .name("bus".into())
            .spawn(move || myos_services::serve_forever(&["log"], |req| handle(&store, req)))
            .expect("spawn bus thread");
    }
    info!("collecting logs on {}", socket.display());
    receive(&sock, &store)
}

fn raise_dgram_queue(want: u64) {
    const P: &str = "/proc/sys/net/unix/max_dgram_qlen";
    let now: u64 = fs::read_to_string(P).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    if now < want {
        // Not fatal: outside the VM (tests) we may not be allowed to.
        let _ = fs::write(P, format!("{want}\n"));
    }
}

fn bind(path: &Path) -> io::Result<UnixDatagram> {
    // A socket left over from an earlier logd is in the way; anything else isn't ours to delete.
    if let Ok(m) = fs::symlink_metadata(path) {
        if m.file_type().is_socket() {
            fs::remove_file(path)?;
        }
    }
    let sock = UnixDatagram::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    let fd = sock.as_raw_fd();
    setsockopt(fd, libc::SO_PASSCRED, 1)?;
    // Room for a burst at boot. The FORCE variant ignores rmem_max (root only).
    if setsockopt(fd, libc::SO_RCVBUFFORCE, 1 << 20).is_err() {
        let _ = setsockopt(fd, libc::SO_RCVBUF, 1 << 20);
    }
    Ok(sock)
}

fn setsockopt(fd: i32, opt: i32, val: i32) -> io::Result<()> {
    // SAFETY: plain setsockopt with an int.
    let r = unsafe {
        libc::setsockopt(fd, libc::SOL_SOCKET, opt, &val as *const i32 as *const c_void, std::mem::size_of::<i32>() as u32)
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Reads one datagram and the sender's credentials (from the kernel, via
/// SO_PASSCRED). Returns the datagram's full length, which is more than
/// `buf.len()` if it was cut.
fn recv_with_creds(fd: i32, buf: &mut [u8]) -> io::Result<(usize, Option<libc::ucred>)> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut c_void, iov_len: buf.len() };
    let mut control = [0u64; 8];
    // SAFETY: msghdr is plain data; all pointers point at live buffers above.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut c_void;
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_TRUNC) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut cred = None;
    // SAFETY: walking the control messages the kernel just wrote.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_CREDENTIALS {
                cred = Some(std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const libc::ucred));
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    Ok((n as usize, cred))
}

fn comm(pid: i32) -> String {
    fs::read_to_string(format!("/proc/{pid}/comm")).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn receive(sock: &UnixDatagram, store: &Shared) -> ExitCode {
    let mut buf = vec![0u8; parse::MAX_MESSAGE + 512];
    loop {
        let (len, cred) = match recv_with_creds(sock.as_raw_fd(), &mut buf) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                error!("reading the log socket: {e}");
                return ExitCode::FAILURE;
            }
        };
        let cut = len > buf.len();
        let m = parse::syslog(&buf[..len.min(buf.len())]);
        let mut e = Entry {
            time_us: now_us(),
            priority: m.priority,
            facility: m.facility,
            tag: m.tag,
            message: m.message,
            ..Default::default()
        };
        match cred {
            // init, passing on a service's output: the tag and pid in the
            // message are the service's, and init vouches for them.
            Some(c) if c.pid == 1 && c.uid == 0 => {
                e.source = "service".into();
                e.pid = m.pid.unwrap_or(0);
            }
            // Anyone else: trust the kernel for who sent it.
            Some(c) => {
                e.source = "syslog".into();
                e.pid = c.pid;
                e.uid = c.uid;
                if e.tag.is_empty() {
                    e.tag = comm(c.pid);
                }
            }
            None => {
                e.source = "syslog".into();
                e.pid = m.pid.unwrap_or(0);
            }
        }
        let mut s = store.lock().unwrap();
        if cut {
            s.malformed += 1;
        }
        s.add(e);
    }
}

/// Kernel messages. /dev/kmsg hands out the whole kernel buffer from the
/// start, so a restarted logd skips what an earlier one logged (by seq,
/// remembered in `state`).
fn read_kmsg(path: &Path, state: Option<&Path>, store: &Shared) {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            warning!("can't read kernel messages from {}: {e}", path.display());
            return;
        }
    };
    let mut done: Option<u64> = state.and_then(|p| fs::read_to_string(p).ok()).and_then(|s| s.trim().parse().ok());
    if let Some(dir) = state.and_then(Path::parent) {
        let _ = fs::create_dir_all(dir);
    }
    let mut buf = vec![0u8; 8192];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            // EPIPE: older records were overwritten before we read them; carry on.
            Err(e) if e.raw_os_error() == Some(libc::EPIPE) || e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                warning!("reading {}: {e}", path.display());
                return;
            }
        };
        let Some(k) = parse::kmsg(&buf[..n]) else { continue };
        if done.is_some_and(|d| k.seq <= d) {
            continue;
        }
        done = Some(k.seq);
        let (tag, pid, message) = if k.facility == 0 {
            ("kernel".to_string(), 0, k.message)
        } else {
            // Something in userspace wrote to /dev/kmsg; it usually has a tag.
            let m = parse::syslog(k.message.as_bytes());
            (if m.tag.is_empty() { "kmsg".into() } else { m.tag }, m.pid.unwrap_or(0), m.message)
        };
        let e = Entry {
            time_us: boot_time_us() + k.ts_us as i64,
            priority: k.priority,
            facility: k.facility,
            source: "kernel".into(),
            tag,
            pid,
            message,
            ..Default::default()
        };
        store.lock().unwrap().add(e);
        if let Some(p) = state {
            let _ = fs::write(p, format!("{}\n", k.seq));
        }
    }
}

/// Wall-clock time of boot, in µs: kernel timestamps count from there.
fn boot_time_us() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime fills ts.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    now_us() - (ts.tv_sec as i64 * 1_000_000 + ts.tv_nsec as i64 / 1000)
}

fn handle(store: &Shared, req: &Request) -> Result<Vec<u8>, Status> {
    match req.method {
        "Write" => {
            let w: WriteRequest = req.decode()?;
            let priority = w.priority.unwrap_or(6);
            if priority > 7 {
                return Err(Status::bad_request("priority must be 0 (emerg) to 7 (debug)"));
            }
            let mut tag = parse::clean(&w.tag);
            tag.retain(|c| !c.is_whitespace() && c != ':' && c != '[');
            tag.truncate(64);
            if tag.is_empty() {
                tag = format!("uid{}", req.caller.uid);
            }
            let e = Entry {
                time_us: now_us(),
                priority,
                facility: if req.caller.uid == 0 { 3 } else { 1 },
                source: "bus".into(),
                tag,
                pid: req.caller.pid,
                uid: req.caller.uid,
                message: parse::clean(&w.message),
                ..Default::default()
            };
            let seq = store.lock().unwrap().add(e);
            reply(&WriteReply { seq })
        }
        "Query" => {
            let q: QueryRequest = req.decode()?;
            reply(&store.lock().unwrap().query(&q, req.caller.uid))
        }
        "Stats" => {
            let _: StatsRequest = req.decode()?;
            reply(&store.lock().unwrap().stats())
        }
        m => Err(Status::unknown_method(m)),
    }
}
