//! sysinfod: system information over the bus (Phase 3), service "sysinfo".
//! See proto/myos/sysinfo/v1/sysinfo.proto. Phase 4's ps and top use it.
//!
//! A sampler thread reads /proc/stat and every /proc/PID/stat once a second,
//! so CPU percentages are ready the moment someone asks.

mod procfs;

use myos_bus::{reply, Request, Status};
use myos_services::proto::sysinfo::*;
use procfs::CpuTimes;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Sample {
    interval_ms: u32,
    /// Differences over the last interval.
    total: CpuTimes,
    per_cpu: Vec<CpuTimes>,
    /// pid -> percent of one CPU
    process_cpu: HashMap<i32, f64>,
}

type Shared = Arc<Mutex<Sample>>;

fn sysconf(name: libc::c_int, fallback: u64) -> u64 {
    // SAFETY: sysconf has no preconditions.
    let v = unsafe { libc::sysconf(name) };
    if v > 0 {
        v as u64
    } else {
        fallback
    }
}

fn clk_tck() -> u64 {
    sysconf(libc::_SC_CLK_TCK, 100)
}

fn pids() -> Vec<i32> {
    let Ok(dir) = fs::read_dir("/proc") else { return Vec::new() };
    let mut v: Vec<i32> = dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect();
    v.sort_unstable();
    v
}

/// pid -> (utime + stime, starttime); starttime tells a reused pid apart.
fn process_ticks() -> HashMap<i32, (u64, u64)> {
    pids()
        .into_iter()
        .filter_map(|pid| {
            let s = procfs::parse_pid_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
            Some((pid, (s.utime + s.stime, s.starttime)))
        })
        .collect()
}

fn sampler(shared: Shared) {
    let tck = clk_tck() as f64;
    let read = || procfs::parse_stat(&fs::read_to_string("/proc/stat").unwrap_or_default());
    let (mut prev_total, mut prev_per) = read();
    let mut prev_procs = process_ticks();
    let mut prev_at = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let (total, per) = read();
        let procs = process_ticks();
        let now = Instant::now();
        let secs = (now - prev_at).as_secs_f64().max(0.001);
        let diff = |a: &CpuTimes, b: &CpuTimes| CpuTimes {
            user: a.user.saturating_sub(b.user),
            system: a.system.saturating_sub(b.system),
            idle: a.idle.saturating_sub(b.idle),
            iowait: a.iowait.saturating_sub(b.iowait),
            total: a.total.saturating_sub(b.total),
        };
        let process_cpu = procs
            .iter()
            .filter_map(|(pid, (ticks, start))| {
                let (old, old_start) = prev_procs.get(pid)?;
                (old_start == start).then(|| (*pid, (ticks.saturating_sub(*old) as f64 / tck / secs * 100.0).max(0.0)))
            })
            .collect();
        *shared.lock().unwrap() = Sample {
            interval_ms: (secs * 1000.0) as u32,
            total: diff(&total, &prev_total),
            per_cpu: per.iter().zip(prev_per.iter().chain(std::iter::repeat(&CpuTimes::default()))).map(|(a, b)| diff(a, b)).collect(),
            process_cpu,
        };
        (prev_total, prev_per, prev_procs, prev_at) = (total, per, procs, now);
    }
}

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn uname() -> (String, String, String) {
    // SAFETY: uname fills the struct; the fields are NUL-terminated.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return Default::default();
    }
    let s = |f: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(f.as_ptr()) }.to_string_lossy().into_owned();
    (s(&u.nodename), s(&u.release), s(&u.machine))
}

fn system() -> SystemReply {
    let (hostname, kernel_release, machine) = uname();
    let uptime = read("/proc/uptime").split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let boot_time = read("/proc/stat").lines().find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok()).unwrap_or(0);
    let la = read("/proc/loadavg");
    let f: Vec<&str> = la.split_whitespace().collect();
    let load = |i: usize| f.get(i).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    // "running/total" scheduling entities (threads included).
    let running = f.get(3).and_then(|v| v.split_once('/')).map_or("0", |(r, _)| r);
    SystemReply {
        hostname,
        os_name: procfs::os_name(&read("/etc/os-release")),
        kernel_release,
        machine,
        uptime_seconds: uptime,
        boot_time,
        load1: load(0),
        load5: load(1),
        load15: load(2),
        cpu_count: sysconf(libc::_SC_NPROCESSORS_ONLN, 1) as u32,
        // loadavg counts threads; count processes ourselves.
        processes: pids().len() as u32,
        running: running.parse().unwrap_or(0),
    }
}

fn cpu(shared: &Shared) -> CpuReply {
    let s = shared.lock().unwrap();
    let t = &s.total;
    let pct = |x: u64| procfs::percent(x, t.total);
    CpuReply {
        model: procfs::cpu_model(&read("/proc/cpuinfo")),
        count: sysconf(libc::_SC_NPROCESSORS_ONLN, 1) as u32,
        usage_percent: pct(t.busy()),
        per_cpu_percent: s.per_cpu.iter().map(|c| procfs::percent(c.busy(), c.total)).collect(),
        user_percent: pct(t.user),
        system_percent: pct(t.system),
        iowait_percent: pct(t.iowait),
        idle_percent: pct(t.idle),
        sample_ms: s.interval_ms,
    }
}

fn processes(shared: &Shared) -> ProcessesReply {
    let users = procfs::parse_passwd(&read("/etc/passwd"));
    let tck = clk_tck() as f64;
    let page_kb = sysconf(libc::_SC_PAGESIZE, 4096) / 1024;
    let cpu = shared.lock().unwrap().process_cpu.clone();
    let processes = pids()
        .into_iter()
        .filter_map(|pid| {
            let st = procfs::parse_pid_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
            let uid = procfs::parse_status_uid(&read(&format!("/proc/{pid}/status"))).unwrap_or(0);
            Some(Process {
                pid,
                ppid: st.ppid,
                uid,
                user: users.get(&uid).cloned().unwrap_or_else(|| uid.to_string()),
                cmdline: procfs::parse_cmdline(&fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default()),
                name: st.comm,
                state: st.state,
                threads: st.threads,
                rss_kb: st.rss_pages * page_kb,
                vsize_kb: st.vsize / 1024,
                cpu_percent: cpu.get(&pid).copied().unwrap_or(0.0),
                start_seconds: st.starttime as f64 / tck,
                cpu_time_ms: ((st.utime + st.stime) as f64 * 1000.0 / tck) as u64,
                nice: st.nice,
            })
        })
        .collect();
    ProcessesReply { processes, memory_total_kb: procfs::parse_meminfo(&read("/proc/meminfo")).total_kb }
}

fn handle(shared: &Shared, req: &Request) -> Result<Vec<u8>, Status> {
    match req.method {
        "System" => {
            let _: SystemRequest = req.decode()?;
            reply(&system())
        }
        "Cpu" => {
            let _: CpuRequest = req.decode()?;
            reply(&cpu(shared))
        }
        "Memory" => {
            let _: MemoryRequest = req.decode()?;
            reply(&procfs::parse_meminfo(&read("/proc/meminfo")))
        }
        "Disks" => {
            let r: DisksRequest = req.decode()?;
            reply(&DisksReply { disks: procfs::disks(&read("/proc/mounts"), r.all) })
        }
        "Processes" => {
            let _: ProcessesRequest = req.decode()?;
            reply(&processes(shared))
        }
        m => Err(Status::unknown_method(m)),
    }
}

fn main() {
    let shared: Shared = Arc::default();
    {
        let shared = shared.clone();
        std::thread::Builder::new().name("sampler".into()).spawn(move || sampler(shared)).expect("spawn sampler");
    }
    myos_services::serve_forever(&["sysinfo"], |req| handle(&shared, req))
}
