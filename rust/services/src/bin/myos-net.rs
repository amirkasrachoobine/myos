//! myos-net: network status and control (netd, bus service "net").

use myos_bus::{Client, Code};
use myos_services::human_duration;
use myos_services::proto::net::{ListInterfacesReply, ListInterfacesRequest, RenewReply, RenewRequest};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: myos-net [status]              interfaces, addresses, leases
       myos-net wait [--timeout SECS]   wait until the network is configured (default 15 s)
       myos-net renew INTERFACE         renew a DHCP lease now (root only)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match words.as_slice() {
        [] | ["status"] => status(),
        ["wait"] => wait(Duration::from_secs(15)),
        ["wait", "--timeout", t] => match t.parse::<f64>() {
            Ok(s) if s >= 0.0 => wait(Duration::from_secs_f64(s)),
            _ => Err(format!("usage: --timeout needs a number of seconds\n{USAGE}")),
        },
        ["renew", iface] => renew(iface),
        ["-h" | "--help"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => Err(format!("usage: unknown arguments\n{USAGE}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("myos-net: {e}");
            ExitCode::FAILURE
        }
    }
}

fn list() -> Result<ListInterfacesReply, myos_bus::Error> {
    Client::connect()?.call("net", "ListInterfaces", &ListInterfacesRequest {})
}

fn status() -> Result<(), String> {
    let r = list().map_err(|e| format!("{e} (is netd running?)"))?;
    for i in &r.interfaces {
        let addrs: Vec<String> = i.addresses.iter().map(|a| format!("{}/{}", a.address, a.prefix_len)).collect();
        let link = if !i.up { "down" } else if i.carrier { "up" } else { "no-carrier" };
        println!(
            "{:<8} {:<10} {:<9} {:<18} {}",
            i.name,
            link,
            i.method,
            if addrs.is_empty() { "-".into() } else { addrs.join(",") },
            if i.state.is_empty() { "-" } else { &i.state }
        );
        let mut extra = Vec::new();
        if !i.gateway.is_empty() {
            extra.push(format!("gateway {}", i.gateway));
        }
        if !i.dns.is_empty() {
            extra.push(format!("dns {}", i.dns.join(" ")));
        }
        if i.lease_seconds > 0 {
            extra.push(format!(
                "lease from {}, {} of {} left",
                i.dhcp_server,
                human_duration(i.lease_remaining_seconds as u64),
                human_duration(i.lease_seconds as u64)
            ));
        }
        if !i.mac.is_empty() && i.name != "lo" {
            extra.push(format!("mac {}", i.mac));
        }
        if !extra.is_empty() {
            println!("         {}", extra.join(", "));
        }
    }
    println!("{}", if r.online { "online" } else { "offline" });
    Ok(())
}

/// Polls netd until it says online. netd (or the bus) may not be up yet at
/// boot, so connection errors just mean "not yet".
fn wait(timeout: Duration) -> Result<(), String> {
    let end = Instant::now() + timeout;
    let mut last;
    loop {
        match list() {
            Ok(r) if r.online => {
                let addrs: Vec<String> = r
                    .interfaces
                    .iter()
                    .filter(|i| i.method != "loopback" && i.configured)
                    .flat_map(|i| i.addresses.iter().map(move |a| format!("{} {}/{}", i.name, a.address, a.prefix_len)))
                    .collect();
                println!("online: {}", addrs.join(", "));
                return Ok(());
            }
            Ok(r) => {
                let pending: Vec<String> = r
                    .interfaces
                    .iter()
                    .filter(|i| i.method != "loopback" && i.method != "unmanaged" && !i.configured)
                    .map(|i| format!("{}: {}", i.name, i.state))
                    .collect();
                last = if pending.is_empty() { "no interfaces to configure".into() } else { pending.join("; ") };
            }
            Err(e) => last = format!("netd not reachable: {e}"),
        }
        if Instant::now() >= end {
            return Err(format!("not online after {}s ({last})", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn renew(iface: &str) -> Result<(), String> {
    let r: Result<RenewReply, _> =
        Client::connect().and_then(|mut c| c.call("net", "Renew", &RenewRequest { interface: iface.into() }));
    match r {
        Ok(_) => {
            println!("renewing {iface}; see `myos-net status`");
            Ok(())
        }
        Err(e) if e.code() == Some(Code::PermissionDenied) => Err("only root may renew leases".into()),
        Err(e) => Err(e.to_string()),
    }
}
