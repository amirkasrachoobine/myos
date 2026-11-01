//! Reading /proc, /etc/passwd and friends. Parsers take text so they can be
//! tested without a live /proc.

use myos_services::proto::sysinfo::{Disk, MemoryReply};
use std::collections::HashMap;

/// One line of /proc/stat: cpu or cpuN, in clock ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub user: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub total: u64,
}

impl CpuTimes {
    pub fn busy(&self) -> u64 {
        self.total.saturating_sub(self.idle + self.iowait)
    }
}

/// /proc/stat: (total, per-cpu).
pub fn parse_stat(text: &str) -> (CpuTimes, Vec<CpuTimes>) {
    let mut total = CpuTimes::default();
    let mut per = Vec::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let Some(name) = f.next() else { continue };
        if !name.starts_with("cpu") {
            continue;
        }
        // user nice system idle iowait irq softirq steal (guest is already in user)
        let v: Vec<u64> = f.take(8).map(|x| x.parse().unwrap_or(0)).collect();
        let get = |i: usize| v.get(i).copied().unwrap_or(0);
        let t = CpuTimes {
            user: get(0) + get(1),
            system: get(2) + get(5) + get(6),
            idle: get(3),
            iowait: get(4),
            total: v.iter().sum(),
        };
        if name == "cpu" {
            total = t;
        } else {
            per.push(t);
        }
    }
    (total, per)
}

pub fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 * 100.0 / whole as f64).clamp(0.0, 100.0)
    }
}

pub fn parse_meminfo(text: &str) -> MemoryReply {
    let mut m: HashMap<&str, u64> = HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if let Some(n) = v.split_whitespace().next().and_then(|n| n.parse().ok()) {
                m.insert(k.trim(), n);
            }
        }
    }
    let g = |k: &str| m.get(k).copied().unwrap_or(0);
    let total = g("MemTotal");
    // MemAvailable exists since Linux 3.14; estimate for anything older.
    let available = m.get("MemAvailable").copied().unwrap_or(g("MemFree") + g("Buffers") + g("Cached"));
    MemoryReply {
        total_kb: total,
        free_kb: g("MemFree"),
        available_kb: available,
        used_kb: total.saturating_sub(available),
        buffers_kb: g("Buffers"),
        cached_kb: g("Cached") + g("SReclaimable"),
        shared_kb: g("Shmem"),
        swap_total_kb: g("SwapTotal"),
        swap_free_kb: g("SwapFree"),
    }
}

/// The fields of /proc/PID/stat that we use. `comm` may contain spaces and
/// parentheses, so it's everything between the first '(' and the last ')'.
#[derive(Debug, Default, PartialEq)]
pub struct PidStat {
    pub comm: String,
    pub state: String,
    pub ppid: i32,
    pub utime: u64,
    pub stime: u64,
    pub nice: i32,
    pub threads: u32,
    pub starttime: u64,
    pub vsize: u64,
    pub rss_pages: u64,
}

pub fn parse_pid_stat(text: &str) -> Option<PidStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?.to_string();
    // Fields after comm, numbered from 3 (state) as in proc(5).
    let f: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let n = |field: usize| f.get(field - 3).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    Some(PidStat {
        comm,
        state: f.first()?.to_string(),
        ppid: n(4) as i32,
        utime: n(14) as u64,
        stime: n(15) as u64,
        nice: n(19) as i32,
        threads: n(20) as u32,
        starttime: n(22) as u64,
        vsize: n(23) as u64,
        rss_pages: n(24).max(0) as u64,
    })
}

/// The real uid from /proc/PID/status.
pub fn parse_status_uid(text: &str) -> Option<u32> {
    text.lines().find_map(|l| l.strip_prefix("Uid:")).and_then(|r| r.split_whitespace().next()).and_then(|u| u.parse().ok())
}

pub fn parse_cmdline(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.trim_end_matches('\0').replace('\0', " ")
}

pub fn parse_passwd(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            Some((f.get(2)?.parse().ok()?, f.first()?.to_string()))
        })
        .collect()
}

pub fn cpu_model(cpuinfo: &str) -> String {
    cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name").and_then(|r| r.split_once(':')).map(|(_, v)| v.trim().to_string()))
        .unwrap_or_default()
}

/// PRETTY_NAME (else NAME) from /etc/os-release.
pub fn os_name(text: &str) -> String {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .map(|v| v.trim().trim_matches('"').to_string())
    };
    get("PRETTY_NAME").or_else(|| get("NAME")).unwrap_or_else(|| "Linux".into())
}

const PSEUDO_FS: &[&str] = &[
    "proc", "sysfs", "devtmpfs", "devpts", "tmpfs", "cgroup", "cgroup2", "securityfs", "debugfs", "tracefs",
    "pstore", "bpf", "mqueue", "hugetlbfs", "configfs", "fusectl", "autofs", "binfmt_misc", "efivarfs", "ramfs",
    "rpc_pipefs", "nsfs", "selinuxfs",
];

/// /proc/mounts escapes spaces and the like as \040 etc.
fn unescape_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (device, mount point, fs type, read-only) for each mount worth showing.
pub fn parse_mounts(text: &str, all: bool) -> Vec<(String, String, String, bool)> {
    let mut out: Vec<(String, String, String, bool)> = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let (dev, mnt, fs, opts) = (unescape_octal(f[0]), unescape_octal(f[1]), f[2].to_string(), f[3]);
        if !all && PSEUDO_FS.contains(&fs.as_str()) {
            continue;
        }
        let ro = opts.split(',').any(|o| o == "ro");
        // Mounted over: the later mount is the one you see.
        out.retain(|m| m.1 != mnt);
        out.push((dev, mnt, fs, ro));
    }
    out
}

pub fn statvfs(path: &str) -> Option<(u64, u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: statvfs fills the struct.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = if s.f_frsize > 0 { s.f_frsize } else { s.f_bsize } as u64;
    let total = s.f_blocks as u64 * frsize;
    let free = s.f_bfree as u64 * frsize;
    let avail = s.f_bavail as u64 * frsize;
    Some((total, total.saturating_sub(free), avail))
}

pub fn disks(mounts: &str, all: bool) -> Vec<Disk> {
    parse_mounts(mounts, all)
        .into_iter()
        .filter_map(|(device, mount_point, fs_type, read_only)| {
            let (total, used, avail) = statvfs(&mount_point)?;
            Some(Disk { mount_point, device, fs_type, total_bytes: total, used_bytes: used, available_bytes: avail, read_only })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat() {
        let (t, per) = parse_stat(
            "cpu  100 5 50 800 10 3 2 0 0 0\ncpu0 60 5 30 400 5 2 1 0 0 0\ncpu1 40 0 20 400 5 1 1 0 0 0\nintr 1 2 3\n",
        );
        assert_eq!(t, CpuTimes { user: 105, system: 55, idle: 800, iowait: 10, total: 970 });
        assert_eq!(per.len(), 2);
        assert_eq!(t.busy(), 160);
        assert_eq!(percent(1, 4), 25.0);
        assert_eq!(percent(1, 0), 0.0);
    }

    #[test]
    fn meminfo() {
        let m = parse_meminfo(
            "MemTotal:        1000 kB\nMemFree:          200 kB\nMemAvailable:     600 kB\nBuffers:           10 kB\nCached:           300 kB\nSReclaimable:      20 kB\nShmem: 5 kB\nSwapTotal: 0 kB\n",
        );
        assert_eq!((m.total_kb, m.available_kb, m.used_kb, m.cached_kb, m.shared_kb), (1000, 600, 400, 320, 5));
    }

    #[test]
    fn pid_stat_with_awkward_comm() {
        let s = parse_pid_stat("42 (my (odd) prog) S 1 42 42 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 2 0 1234 10485760 256 18446744073709551615").unwrap();
        assert_eq!(s.comm, "my (odd) prog");
        assert_eq!((s.state.as_str(), s.ppid, s.utime, s.stime, s.nice, s.threads), ("S", 1, 7, 3, 0, 2));
        assert_eq!((s.starttime, s.vsize, s.rss_pages), (1234, 10_485_760, 256));
    }

    #[test]
    fn small_files() {
        assert_eq!(parse_status_uid("Name:\tsh\nUid:\t1000\t1000\t1000\t1000\n"), Some(1000));
        assert_eq!(parse_cmdline(b"/sbin/getty\0-L\0ttyS0\0"), "/sbin/getty -L ttyS0");
        assert_eq!(parse_passwd("root:x:0:0:root:/root:/bin/sh\nbad\n").get(&0).map(String::as_str), Some("root"));
        assert_eq!(cpu_model("processor\t: 0\nmodel name\t: QEMU Virtual CPU version 2.5+\n"), "QEMU Virtual CPU version 2.5+");
        assert_eq!(os_name("NAME=Buildroot\nPRETTY_NAME=\"Buildroot 2026.02\"\n"), "Buildroot 2026.02");
        assert_eq!(os_name("NAME=Buildroot\n"), "Buildroot");
    }

    #[test]
    fn mounts() {
        let m = parse_mounts(
            "/dev/root / ext4 rw,relatime 0 0\nproc /proc proc rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n/dev/vdb /mnt/my\\040disk ext4 ro 0 0\n",
            false,
        );
        assert_eq!(m, vec![
            ("/dev/root".into(), "/".into(), "ext4".into(), false),
            ("/dev/vdb".into(), "/mnt/my disk".into(), "ext4".into(), true),
        ]);
        assert_eq!(parse_mounts("proc /proc proc rw 0 0\n", true).len(), 1);
    }
}
