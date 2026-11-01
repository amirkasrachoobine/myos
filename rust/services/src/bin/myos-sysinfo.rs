//! myos-sysinfo: what sysinfod reports (bus service "sysinfo").

use myos_bus::Client;
use myos_services::proto::sysinfo::*;
use myos_services::{human_bytes, human_duration};
use std::process::ExitCode;

const USAGE: &str = "\
usage: myos-sysinfo [summary]    system, CPU, memory and disks at a glance
       myos-sysinfo cpu
       myos-sysinfo mem
       myos-sysinfo disks [-a]   -a: include pseudo filesystems (proc, tmpfs, ...)
       myos-sysinfo ps           processes, as the Phase 4 ps will show them";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if matches!(words.as_slice(), ["-h" | "--help"]) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let mut bus = match Client::connect() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("myos-sysinfo: {e} (is busd running?)");
            return ExitCode::FAILURE;
        }
    };
    let r = match words.as_slice() {
        [] | ["summary"] => summary(&mut bus),
        ["cpu"] => cpu(&mut bus),
        ["mem"] => mem(&mut bus),
        ["disks"] => disks(&mut bus, false),
        ["disks", "-a"] => disks(&mut bus, true),
        ["ps"] => ps(&mut bus),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("myos-sysinfo: {e}");
            ExitCode::FAILURE
        }
    }
}

type R = Result<(), myos_bus::Error>;

fn kib(kb: u64) -> String {
    human_bytes(kb * 1024)
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

fn summary(bus: &mut Client) -> R {
    let s: SystemReply = bus.call("sysinfo", "System", &SystemRequest {})?;
    let c: CpuReply = bus.call("sysinfo", "Cpu", &CpuRequest {})?;
    let m: MemoryReply = bus.call("sysinfo", "Memory", &MemoryRequest {})?;
    let d: DisksReply = bus.call("sysinfo", "Disks", &DisksRequest { all: false })?;
    println!(
        "system  {} ({}), Linux {} {}, up {}, load {:.2} {:.2} {:.2}, {} processes",
        s.hostname,
        s.os_name,
        s.kernel_release,
        s.machine,
        human_duration(s.uptime_seconds as u64),
        s.load1,
        s.load5,
        s.load15,
        s.processes
    );
    println!("cpu     {} x {}, {:.1}% used", c.count, if c.model.is_empty() { "CPU" } else { &c.model }, c.usage_percent);
    println!("memory  {} used of {} ({:.0}%), {} available", kib(m.used_kb), kib(m.total_kb), pct(m.used_kb, m.total_kb), kib(m.available_kb));
    for disk in &d.disks {
        println!(
            "disk    {} {} used of {} ({:.0}%), {}{}",
            disk.mount_point,
            human_bytes(disk.used_bytes),
            human_bytes(disk.total_bytes),
            pct(disk.used_bytes, disk.total_bytes),
            disk.fs_type,
            if disk.read_only { ", read-only" } else { "" }
        );
    }
    Ok(())
}

fn cpu(bus: &mut Client) -> R {
    let c: CpuReply = bus.call("sysinfo", "Cpu", &CpuRequest {})?;
    println!("model    {}", c.model);
    println!("cpus     {}", c.count);
    println!(
        "usage    {:.1}%  (user {:.1}%, system {:.1}%, iowait {:.1}%, idle {:.1}%)",
        c.usage_percent, c.user_percent, c.system_percent, c.iowait_percent, c.idle_percent
    );
    for (i, p) in c.per_cpu_percent.iter().enumerate() {
        println!("cpu{i:<5} {p:.1}%");
    }
    println!("sampled  over {} ms", c.sample_ms);
    Ok(())
}

fn mem(bus: &mut Client) -> R {
    let m: MemoryReply = bus.call("sysinfo", "Memory", &MemoryRequest {})?;
    for (label, v) in [
        ("total", m.total_kb),
        ("used", m.used_kb),
        ("free", m.free_kb),
        ("available", m.available_kb),
        ("buffers", m.buffers_kb),
        ("cached", m.cached_kb),
        ("shared", m.shared_kb),
        ("swap total", m.swap_total_kb),
        ("swap free", m.swap_free_kb),
    ] {
        println!("{label:<11} {:>10}", kib(v));
    }
    Ok(())
}

fn disks(bus: &mut Client, all: bool) -> R {
    let d: DisksReply = bus.call("sysinfo", "Disks", &DisksRequest { all })?;
    println!("{:<20} {:<10} {:>10} {:>10} {:>10} {:>5}  DEVICE", "MOUNTED ON", "TYPE", "SIZE", "USED", "AVAIL", "USE%");
    for x in &d.disks {
        println!(
            "{:<20} {:<10} {:>10} {:>10} {:>10} {:>4.0}%  {}{}",
            x.mount_point,
            x.fs_type,
            human_bytes(x.total_bytes),
            human_bytes(x.used_bytes),
            human_bytes(x.available_bytes),
            pct(x.used_bytes, x.total_bytes),
            x.device,
            if x.read_only { " (ro)" } else { "" }
        );
    }
    Ok(())
}

fn ps(bus: &mut Client) -> R {
    let r: ProcessesReply = bus.call("sysinfo", "Processes", &ProcessesRequest {})?;
    println!("{:>6} {:>6} {:<8} {:>1} {:>5} {:>9} {:>8}  COMMAND", "PID", "PPID", "USER", "S", "%CPU", "RSS", "TIME");
    for p in &r.processes {
        let cmd = if p.cmdline.is_empty() { format!("[{}]", p.name) } else { p.cmdline.clone() };
        let t = p.cpu_time_ms / 1000;
        println!(
            "{:>6} {:>6} {:<8} {:>1} {:>5.1} {:>9} {:>5}:{:02}  {}",
            p.pid,
            p.ppid,
            p.user,
            p.state,
            p.cpu_percent,
            kib(p.rss_kb),
            t / 60,
            t % 60,
            cmd
        );
    }
    Ok(())
}
