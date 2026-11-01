//! Early boot: API filesystems, console, hostname. Every step logs and
//! carries on if it fails; a half-mounted system with a login prompt is far
//! more useful than a kernel panic.

use crate::log;
use crate::sys::{self, MS_NODEV, MS_NOEXEC, MS_NOSUID, MS_RDONLY};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;

/// (source, target, fstype, flags, options)
const MOUNTS: &[(&str, &str, &str, u64, &str)] = &[
    ("proc", "/proc", "proc", MS_NOSUID | MS_NODEV | MS_NOEXEC, ""),
    ("sysfs", "/sys", "sysfs", MS_NOSUID | MS_NODEV | MS_NOEXEC, ""),
    ("devtmpfs", "/dev", "devtmpfs", MS_NOSUID, "mode=0755"),
    ("devpts", "/dev/pts", "devpts", MS_NOSUID | MS_NOEXEC, "gid=5,mode=0620,ptmxmode=0666"),
    ("tmpfs", "/dev/shm", "tmpfs", MS_NOSUID | MS_NODEV, "mode=1777"),
    ("tmpfs", "/run", "tmpfs", MS_NOSUID | MS_NODEV, "mode=0755"),
    ("tmpfs", "/tmp", "tmpfs", MS_NOSUID | MS_NODEV, "mode=1777"),
];

/// Mount points currently in /proc/self/mountinfo (empty before /proc is up).
fn mounted() -> Vec<String> {
    fs::read_to_string("/proc/self/mountinfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split(' ').nth(4).map(str::to_string))
        .collect()
}

pub fn mount_api_filesystems() {
    for &(src, target, fstype, flags, opts) in MOUNTS {
        // The kernel may have mounted /dev already (CONFIG_DEVTMPFS_MOUNT);
        // mounting again would stack a second copy on top.
        if mounted().iter().any(|m| m == target) {
            continue;
        }
        if let Err(e) = fs::create_dir_all(target) {
            log!("cannot create {target}: {e}");
            continue;
        }
        if let Err(e) = sys::do_mount(src, target, fstype, flags as _, opts) {
            log!("mount {fstype} on {target} failed: {e}");
        }
    }
    // devtmpfs doesn't provide these; lots of scripts expect them.
    for (link, to) in [
        ("/dev/fd", "/proc/self/fd"),
        ("/dev/stdin", "/proc/self/fd/0"),
        ("/dev/stdout", "/proc/self/fd/1"),
        ("/dev/stderr", "/proc/self/fd/2"),
    ] {
        if fs::symlink_metadata(link).is_err() {
            let _ = std::os::unix::fs::symlink(to, link);
        }
    }
    // /var/run and /var/lock in the rootfs overlay point at these.
    for dir in ["/run/myos", "/run/lock"] {
        let _ = fs::create_dir_all(dir);
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    }
}

/// Point fds 0–2 at /dev/console. Opened with O_NOCTTY: PID 1 must never
/// acquire a controlling terminal, or a Ctrl-C on the console would hit it.
pub fn attach_console() {
    match OpenOptions::new().read(true).write(true).custom_flags(sys::O_NOCTTY).open("/dev/console") {
        Ok(f) => {
            for fd in 0..3 {
                let _ = sys::dup_onto(f.as_raw_fd(), fd);
            }
        }
        Err(e) => log!("cannot open /dev/console: {e}"),
    }
}

pub fn remount_root_rw() {
    if skip_root_remount() {
        return;
    }
    if let Err(e) = sys::remount("/", 0) {
        log!("remounting / read-write failed: {e}");
    }
}

pub fn remount_root_ro() {
    if skip_root_remount() {
        return;
    }
    if let Err(e) = sys::remount("/", MS_RDONLY) {
        log!("remounting / read-only failed: {e}");
    }
}

/// Test hook: when init runs in a container sharing the host's root
/// filesystem, remounting / would change the host's mount too.
fn skip_root_remount() -> bool {
    std::env::var_os("MYOS_INIT_NO_ROOT_REMOUNT").is_some()
}

pub fn set_hostname() {
    let name = fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let name = name.unwrap_or_else(|| "myos".to_string());
    match sys::set_hostname(&name) {
        Ok(()) => log!("hostname set to {name}"),
        Err(e) => log!("cannot set hostname to {name}: {e}"),
    }
}
