//! myos-log: read and write the system log (logd, bus service "log").

use myos_bus::Client;
use myos_services::proto::log::{Entry, QueryReply, QueryRequest, StatsReply, StatsRequest, WriteReply, WriteRequest};
use myos_services::{format_time_us, human_bytes, parse_priority, priority_name};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
usage: myos-log [-n LINES] [-f] [-t TAG] [-p PRIORITY] [-g TEXT] [-s]
       myos-log write [-t TAG] [-p PRIORITY] MESSAGE...
       myos-log stats

  -n LINES     show the last LINES entries (default 50, at most 1000)
  -f           keep printing new entries as they arrive (Ctrl-C to stop)
  -t TAG       only this service or program, e.g. netd, kernel
  -p PRIORITY  only this severe or worse: emerg alert crit err warning notice info debug
  -g TEXT      only messages containing TEXT
  -s           also show where each entry came from (service, syslog, kernel, bus)

Times are UTC. Older entries than logd keeps in memory are in /var/log/messages.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("write") => write(&args[1..]),
        Some("stats") => stats(),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => show(&args),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("myos-log: {e}");
            if e.starts_with("usage") {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn connect() -> Result<Client, String> {
    Client::connect().map_err(|e| format!("{e} (is busd running?)"))
}

fn value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<&'a String, String> {
    it.next().ok_or_else(|| format!("usage: {flag} needs a value\n{USAGE}"))
}

fn priority(s: &str) -> Result<u32, String> {
    parse_priority(s).ok_or_else(|| format!("unknown priority {s:?} (emerg alert crit err warning notice info debug)"))
}

fn show(args: &[String]) -> Result<(), String> {
    let mut q = QueryRequest { limit: 50, ..Default::default() };
    let (mut follow, mut sources) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" => q.limit = value(&mut it, "-n")?.parse().map_err(|_| format!("usage: -n needs a number\n{USAGE}"))?,
            "-f" => follow = true,
            "-s" => sources = true,
            "-t" => q.tag = value(&mut it, "-t")?.clone(),
            "-p" => q.max_priority = Some(priority(value(&mut it, "-p")?)?),
            "-g" => q.contains = value(&mut it, "-g")?.clone(),
            _ => return Err(format!("usage: unknown argument {a:?}\n{USAGE}")),
        }
    }
    q.limit = q.limit.min(1000);
    let mut bus = connect()?;
    let print = |r: &QueryReply| {
        for e in &r.entries {
            println!("{}", line(e, sources));
        }
    };
    if q.limit > 0 {
        let r: QueryReply = bus.call("log", "Query", &q).map_err(|e| e.to_string())?;
        print(&r);
        q.after_seq = r.last_seq;
    } else {
        let r: QueryReply = bus.call("log", "Query", &QueryRequest { limit: 1, ..Default::default() }).map_err(|e| e.to_string())?;
        q.after_seq = r.last_seq;
    }
    if !follow {
        return Ok(());
    }
    q.limit = 1000;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let r: QueryReply = match bus.call("log", "Query", &q) {
            Ok(r) => r,
            Err(e) => {
                // logd or busd restarted: reconnect and carry on.
                eprintln!("myos-log: {e}; reconnecting");
                std::thread::sleep(Duration::from_secs(1));
                if let Ok(b) = Client::connect() {
                    bus = b;
                }
                continue;
            }
        };
        if r.last_seq < q.after_seq {
            // A new logd started counting from 1 again.
            q.after_seq = 0;
            continue;
        }
        print(&r);
        // Skip past everything seen, matching or not, unless there's more to page through.
        q.after_seq = if r.entries.len() as u32 >= q.limit { r.entries.last().map_or(r.last_seq, |e| e.seq) } else { r.last_seq };
    }
}

fn line(e: &Entry, sources: bool) -> String {
    let who = match (e.tag.is_empty(), e.pid) {
        (true, _) => "-".to_string(),
        (false, 0) => e.tag.clone(),
        (false, p) => format!("{}[{p}]", e.tag),
    };
    let src = if sources { format!("{:<8} ", e.source) } else { String::new() };
    format!("{} {src}{:<7} {who}: {}", format_time_us(e.time_us), priority_name(e.priority), e.message)
}

fn write(args: &[String]) -> Result<(), String> {
    let mut w = WriteRequest::default();
    let mut words = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-t" => w.tag = value(&mut it, "-t")?.clone(),
            "-p" => w.priority = Some(priority(value(&mut it, "-p")?)?),
            _ => words.push(a.as_str()),
        }
    }
    if words.is_empty() {
        return Err(format!("usage: write needs a message\n{USAGE}"));
    }
    w.message = words.join(" ");
    if w.tag.is_empty() {
        w.tag = std::env::var("USER").unwrap_or_else(|_| "myos-log".into());
    }
    let r: WriteReply = connect()?.call("log", "Write", &w).map_err(|e| e.to_string())?;
    println!("logged as entry {}", r.seq);
    Ok(())
}

fn stats() -> Result<(), String> {
    let s: StatsReply = connect()?.call("log", "Stats", &StatsRequest {}).map_err(|e| e.to_string())?;
    println!("received   {}", s.received);
    println!("in memory  {} of {} (entries {}..{})", s.in_memory, s.memory_capacity, s.first_seq, s.last_seq);
    if s.file.is_empty() {
        println!("file       none");
    } else {
        println!("file       {} ({})", s.file, human_bytes(s.file_bytes));
    }
    println!("malformed  {}", s.malformed);
    Ok(())
}
