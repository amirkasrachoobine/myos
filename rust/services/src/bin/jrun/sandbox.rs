//! Starting an app: what `jrun run` does between "root" and "the JVM".
//!
//! In order, while still root:
//!   1. new mount namespace, private propagation; fresh tmpfs on /tmp,
//!      /var/tmp and /dev/shm, so apps can't share or snoop files there
//!   2. without the "network" permission: a new network namespace with only
//!      its own loopback, brought up
//!   3. resource limits: open files, processes (per app uid), no core dumps
//!   4. drop to the app's uid and gid, no supplementary groups, and check
//!      that root can't be regained
//!   5. no_new_privs, so setuid programs can't raise it again
//! then chdir to the app's data directory and exec java with a clean
//! environment.

use std::ffi::CString;
use std::io;
use std::path::{Path, PathBuf};

pub struct Plan {
    pub uid: u32,
    pub network: bool,
    pub data: PathBuf,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

fn cstr(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("{s:?} contains a NUL byte"))
}

fn check(r: libc::c_int, what: &str) -> Result<(), String> {
    if r == 0 {
        Ok(())
    } else {
        Err(format!("{what}: {}", io::Error::last_os_error()))
    }
}

fn mount_tmpfs(dir: &str) -> Result<(), String> {
    if !Path::new(dir).is_dir() {
        return Ok(());
    }
    let (src, target, fstype, data) = (cstr("tmpfs")?, cstr(dir)?, cstr("tmpfs")?, cstr("mode=1777,size=64m")?);
    // SAFETY: all pointers are valid NUL-terminated strings.
    let r = unsafe {
        libc::mount(
            src.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            data.as_ptr() as *const libc::c_void,
        )
    };
    check(r, &format!("mounting a private tmpfs on {dir}"))
}

#[repr(C)]
struct IfreqFlags {
    name: [u8; libc::IFNAMSIZ],
    flags: libc::c_short,
    _pad: [u8; 24],
}

fn loopback_up() -> Result<(), String> {
    // SAFETY: plain socket/ioctl calls on a struct laid out like ifreq.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(format!("socket: {}", io::Error::last_os_error()));
        }
        let mut req = IfreqFlags { name: [0; libc::IFNAMSIZ], flags: 0, _pad: [0; 24] };
        req.name[..2].copy_from_slice(b"lo");
        let mut r = libc::ioctl(fd, libc::SIOCGIFFLAGS, &mut req);
        if r == 0 {
            req.flags |= libc::IFF_UP as libc::c_short;
            r = libc::ioctl(fd, libc::SIOCSIFFLAGS, &mut req);
        }
        let err = io::Error::last_os_error();
        libc::close(fd);
        if r != 0 {
            return Err(format!("bringing up lo: {err}"));
        }
    }
    Ok(())
}

fn limit(resource: libc::__rlimit_resource_t, n: u64, what: &str) -> Result<(), String> {
    let l = libc::rlimit { rlim_cur: n, rlim_max: n };
    // SAFETY: passing a valid rlimit.
    check(unsafe { libc::setrlimit(resource, &l) }, &format!("setrlimit {what}"))
}

/// Never returns on success.
pub fn exec(plan: &Plan) -> Result<std::convert::Infallible, String> {
    let argv: Vec<CString> = plan.argv.iter().map(|a| cstr(a)).collect::<Result<_, _>>()?;
    let envp: Vec<CString> = plan.env.iter().map(|(k, v)| cstr(&format!("{k}={v}"))).collect::<Result<_, _>>()?;
    let data = cstr(&plan.data.to_string_lossy())?;
    let mut argv_p: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_p.push(std::ptr::null());
    let mut envp_p: Vec<*const libc::c_char> = envp.iter().map(|a| a.as_ptr()).collect();
    envp_p.push(std::ptr::null());

    // 1, 2: namespaces
    let flags = libc::CLONE_NEWNS | if plan.network { 0 } else { libc::CLONE_NEWNET };
    // SAFETY: unshare has no memory-safety preconditions.
    if unsafe { libc::unshare(flags) } != 0 {
        let e = io::Error::last_os_error();
        if !plan.network && e.raw_os_error() == Some(libc::EINVAL) {
            return Err(format!(
                "can't make a private network namespace ({e}); the kernel needs CONFIG_NET_NS. \
                 Refusing to start an app without the \"network\" permission on the real network."
            ));
        }
        return Err(format!("unshare: {e}"));
    }
    let root = cstr("/")?;
    // SAFETY: valid strings; null source/fstype/data are allowed for MS_PRIVATE.
    let r = unsafe {
        libc::mount(std::ptr::null(), root.as_ptr(), std::ptr::null(), libc::MS_REC | libc::MS_PRIVATE, std::ptr::null())
    };
    check(r, "making mounts private")?;
    for d in ["/tmp", "/var/tmp", "/dev/shm"] {
        mount_tmpfs(d)?;
    }
    if !plan.network {
        loopback_up()?;
    }

    // 3: limits
    limit(libc::RLIMIT_NOFILE, 4096, "RLIMIT_NOFILE")?;
    limit(libc::RLIMIT_NPROC, 1024, "RLIMIT_NPROC")?;
    limit(libc::RLIMIT_CORE, 0, "RLIMIT_CORE")?;

    // 4: drop privileges
    let id = plan.uid;
    // SAFETY: plain syscalls.
    unsafe {
        check(libc::setgroups(0, std::ptr::null()), "setgroups")?;
        check(libc::setresgid(id, id, id), "setresgid")?;
        check(libc::setresuid(id, id, id), "setresuid")?;
        if libc::setuid(0) == 0 || libc::getuid() != id || libc::geteuid() != id || libc::getegid() != id {
            return Err("could still regain root after dropping privileges; not starting the app".into());
        }
        // 5
        check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), "PR_SET_NO_NEW_PRIVS")?;
        libc::umask(0o077);
        check(libc::chdir(data.as_ptr()), &format!("chdir {}", plan.data.display()))?;
        libc::execve(argv_p[0], argv_p.as_ptr(), envp_p.as_ptr());
    }
    Err(format!("can't run {}: {}", plan.argv[0], io::Error::last_os_error()))
}

/// Kills every process running as `uid` (an app's leftovers after it's
/// stopped or removed): forks, becomes that uid, and kill(-1, SIGKILL),
/// which reaches exactly the processes that uid may signal.
pub fn kill_all(uid: u32) {
    // SAFETY: the child only makes async-signal-safe calls and _exits.
    unsafe {
        let pid = libc::fork();
        if pid == 0 {
            if libc::setresgid(uid, uid, uid) == 0 && libc::setresuid(uid, uid, uid) == 0 {
                libc::kill(-1, libc::SIGKILL);
            }
            libc::_exit(0);
        }
        if pid > 0 {
            let mut st = 0;
            libc::waitpid(pid, &mut st, 0);
        }
    }
}
