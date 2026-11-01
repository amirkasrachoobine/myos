//! myos-bus: command-line access to the IPC bus, and the Rust ping client.
//!
//!   myos-bus ping [MESSAGE]     round trip through the broker
//!   myos-bus list               services on the bus

use myos_bus::Client;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: myos-bus [--socket PATH] [--timeout SECONDS] ping [MESSAGE]
       myos-bus [--socket PATH] [--timeout SECONDS] list";

fn main() -> ExitCode {
    let mut socket = None;
    let mut timeout = None;
    let mut rest = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = args.next(),
            "--timeout" => match args.next().and_then(|s| s.parse::<f64>().ok()).filter(|s| *s > 0.0) {
                Some(s) => timeout = Some(Duration::from_secs_f64(s)),
                None => return usage(),
            },
            "-h" | "--help" => return usage(),
            _ => rest.push(a),
        }
    }
    let path = socket.map(Into::into).unwrap_or_else(myos_bus::socket_path);
    let cmd = rest.first().map(String::as_str);
    if !matches!(cmd, Some("ping" | "list")) {
        return usage();
    }

    let mut bus = match Client::connect_to(&path) {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    if let Some(t) = timeout {
        bus.set_timeout(t);
    }

    match cmd {
        Some("ping") => {
            let msg = if rest.len() > 1 { rest[1..].join(" ") } else { "hello from Rust".into() };
            let t0 = Instant::now();
            match bus.ping(&msg) {
                Ok(r) => {
                    println!(
                        "pong: {:?} from {} in {:.2} ms",
                        r.message,
                        r.broker_version,
                        t0.elapsed().as_secs_f64() * 1000.0
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        _ => match bus.list_services() {
            Ok(services) => {
                println!("{:<32} {:>7} {:>6}", "SERVICE", "PID", "UID");
                for s in services {
                    let pid = if s.pid == 0 { "-".to_string() } else { s.pid.to_string() };
                    println!("{:<32} {:>7} {:>6}", s.name, pid, s.uid);
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
    }
}

fn fail(e: myos_bus::Error) -> ExitCode {
    eprintln!("myos-bus: {e}");
    ExitCode::FAILURE
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
