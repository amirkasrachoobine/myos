//! Thin wrappers over the handful of libc calls PID 1 needs that `std`
//! doesn't expose. Declared by hand so the crate has no dependencies.
//!
//! Constants and struct layouts are for Linux on x86_64 / aarch64 / riscv64
//! with glibc. (SIGCHLD etc. differ on mips/alpha/sparc; we don't target those.)

use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int, c_ulong, c_void};

pub const SIGHUP: c_int = 1;
pub const SIGINT: c_int = 2;
pub const SIGQUIT: c_int = 3;
pub const SIGKILL: c_int = 9;
pub const SIGUSR1: c_int = 10;
pub const SIGUSR2: c_int = 12;
pub const SIGTERM: c_int = 15;
pub const SIGCHLD: c_int = 17;
pub const SIGPWR: c_int = 30;

pub const MS_RDONLY: c_ulong = 1;
pub const MS_NOSUID: c_ulong = 2;
pub const MS_NODEV: c_ulong = 4;
pub const MS_NOEXEC: c_ulong = 8;
pub const MS_REMOUNT: c_ulong = 32;

pub const RB_AUTOBOOT: c_int = 0x0123_4567;
pub const RB_HALT_SYSTEM: c_int = 0xcdef_0123_u32 as c_int;
pub const RB_POWER_OFF: c_int = 0x4321_fedc;
pub const RB_DISABLE_CAD: c_int = 0;

pub const O_NOCTTY: c_int = 0o400;

const SIG_SETMASK: c_int = 2;
const SFD_NONBLOCK: c_int = 0o4000;
const SFD_CLOEXEC: c_int = 0o2000000;
const WNOHANG: c_int = 1;
const POLLIN: i16 = 1;

/// glibc's sigset_t: 1024 bits.
#[repr(C)]
pub struct SigSet([u64; 16]);

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

extern "C" {
    fn mount(src: *const c_char, target: *const c_char, fstype: *const c_char, flags: c_ulong, data: *const c_void) -> c_int;
    fn sethostname(name: *const c_char, len: usize) -> c_int;
    fn sigemptyset(set: *mut SigSet) -> c_int;
    fn sigfillset(set: *mut SigSet) -> c_int;
    fn sigaddset(set: *mut SigSet, sig: c_int) -> c_int;
    fn sigprocmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
    fn signalfd(fd: c_int, mask: *const SigSet, flags: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn reboot(cmd: c_int) -> c_int;
    fn sync();
    fn setsid() -> c_int;
    fn getpid() -> c_int;
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn dup2(old: c_int, new: c_int) -> c_int;
}

fn cvt(r: c_int) -> io::Result<c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("path contains NUL")
}

pub fn pid() -> i32 {
    unsafe { getpid() }
}

pub fn do_mount(src: &str, target: &str, fstype: &str, flags: c_ulong, data: &str) -> io::Result<()> {
    let (src, target, fstype, data) = (cstr(src), cstr(target), cstr(fstype), cstr(data));
    let data_ptr = if data.as_bytes().is_empty() { std::ptr::null() } else { data.as_ptr() as *const c_void };
    cvt(unsafe { mount(src.as_ptr(), target.as_ptr(), fstype.as_ptr(), flags, data_ptr) }).map(drop)
}

pub fn remount(target: &str, flags: c_ulong) -> io::Result<()> {
    let target = cstr(target);
    cvt(unsafe { mount(std::ptr::null(), target.as_ptr(), std::ptr::null(), MS_REMOUNT | flags, std::ptr::null()) }).map(drop)
}

pub fn set_hostname(name: &str) -> io::Result<()> {
    cvt(unsafe { sethostname(name.as_ptr() as *const c_char, name.len()) }).map(drop)
}

/// Block every signal in PID 1 and return a non-blocking signalfd that
/// delivers the ones we act on. Blocked signals are queued for PID 1 even
/// when their disposition is SIG_DFL, so nothing sent to us is lost.
pub fn block_signals_and_open_signalfd(wanted: &[c_int]) -> io::Result<c_int> {
    unsafe {
        let mut all = SigSet([0; 16]);
        sigfillset(&mut all);
        cvt(sigprocmask(SIG_SETMASK, &all, std::ptr::null_mut()))?;
        let mut set = SigSet([0; 16]);
        sigemptyset(&mut set);
        for &s in wanted {
            sigaddset(&mut set, s);
        }
        cvt(signalfd(-1, &set, SFD_NONBLOCK | SFD_CLOEXEC))
    }
}

/// Runs in the forked child just before exec: restore a normal signal mask
/// and put the service in its own session / process group, so stopping it
/// can signal the whole group. Only async-signal-safe calls here.
pub fn child_setup() -> io::Result<()> {
    unsafe {
        let mut empty = SigSet([0; 16]);
        sigemptyset(&mut empty);
        sigprocmask(SIG_SETMASK, &empty, std::ptr::null_mut());
        setsid();
    }
    Ok(())
}

/// Drain the signalfd. Returns the signal numbers read.
pub fn read_signals(fd: c_int) -> Vec<c_int> {
    let mut out = Vec::new();
    let mut buf = [0u8; 128 * 8]; // signalfd_siginfo is 128 bytes; ssi_signo is the first u32
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        for rec in buf[..n as usize].chunks_exact(128) {
            out.push(u32::from_ne_bytes([rec[0], rec[1], rec[2], rec[3]]) as c_int);
        }
    }
    out
}

/// Wait until one of `fds` is readable or `timeout_ms` passes (-1 = forever).
/// Returns which fds are readable.
pub fn poll_readable(fds: &[c_int], timeout_ms: i32) -> Vec<bool> {
    let mut pfds: Vec<PollFd> = fds.iter().map(|&fd| PollFd { fd, events: POLLIN, revents: 0 }).collect();
    let r = unsafe { poll(pfds.as_mut_ptr(), pfds.len() as c_ulong, timeout_ms) };
    if r <= 0 {
        return vec![false; fds.len()];
    }
    pfds.iter().map(|p| p.revents != 0).collect()
}

/// Reap one exited child, if any. Returns (pid, raw wait status).
pub fn reap_one() -> Option<(i32, i32)> {
    let mut status = 0;
    let r = unsafe { waitpid(-1, &mut status, WNOHANG) };
    if r > 0 { Some((r, status)) } else { None }
}

/// Block until any child exits. Used only by the emergency shell loop.
pub fn wait_any() -> Option<(i32, i32)> {
    let mut status = 0;
    let r = unsafe { waitpid(-1, &mut status, 0) };
    if r > 0 { Some((r, status)) } else { None }
}

pub fn send_signal(pid: i32, sig: c_int) -> io::Result<()> {
    cvt(unsafe { kill(pid, sig) }).map(drop)
}

/// Signal a service: its whole process group, and the leader itself in case
/// it moved to another group.
pub fn signal_service(pid: i32, sig: c_int) {
    let _ = send_signal(-pid, sig);
    let _ = send_signal(pid, sig);
}

pub fn sync_all() {
    unsafe { sync() }
}

pub fn do_reboot(cmd: c_int) -> io::Result<()> {
    cvt(unsafe { reboot(cmd) }).map(drop)
}

pub fn dup_onto(old: c_int, new: c_int) -> io::Result<()> {
    cvt(unsafe { dup2(old, new) }).map(drop)
}

pub fn signal_from_name(name: &str) -> Option<c_int> {
    let n = name.trim_start_matches("SIG");
    Some(match n {
        "HUP" => SIGHUP,
        "INT" => SIGINT,
        "QUIT" => SIGQUIT,
        "KILL" => SIGKILL,
        "USR1" => SIGUSR1,
        "USR2" => SIGUSR2,
        "TERM" => SIGTERM,
        _ => return None,
    })
}
