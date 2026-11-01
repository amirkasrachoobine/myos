//! netd: the myos network daemon (Phase 3), service "net" on the bus.
//! Replaces `ifup -a` and BusyBox udhcpc.
//!
//! Brings up lo as 127.0.0.1/8, then configures the interfaces listed in
//! /etc/myos/netd.toml (see config.rs) with its own DHCP client or a static
//! address, writes /etc/resolv.conf, and keeps DHCP leases renewed.
//! Interfaces that appear later (hotplug) are picked up within 2 s.
//!
//! Settings, from the environment (for tests):
//!   MYOS_NETD_CONFIG   /etc/myos/netd.toml
//!   MYOS_NETD_RESOLV   /etc/resolv.conf

mod client;
mod config;
mod dhcp;
mod link;

use client::{Ctx, Managed};
use config::{Config, Method};
use myos_bus::{reply, Code, Request, Status};
use myos_services::proto::net::*;
use myos_services::{error, info, warning};
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn load_config(path: &PathBuf) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => match config::parse(&text) {
            Ok(c) => c,
            Err(e) => {
                error!("{}: {e}; using DHCP on every Ethernet interface instead", path.display());
                Config::auto()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("no {}: using DHCP on every Ethernet interface", path.display());
            Config::auto()
        }
        Err(e) => {
            error!("can't read {}: {e}; using DHCP on every Ethernet interface", path.display());
            Config::auto()
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_default()
}

fn loopback(ctx: &Ctx) {
    let r = link::set_up("lo").and_then(|_| {
        if link::addresses().get("lo").map_or(true, |a| !a.contains(&(Ipv4Addr::LOCALHOST, 8))) {
            link::set_addr("lo", Ipv4Addr::LOCALHOST, 8)
        } else {
            Ok(())
        }
    });
    let (configured, state) = match r {
        Ok(()) => (true, "configured".to_string()),
        Err(e) => {
            error!("lo: {e}");
            (false, format!("failed: {e}"))
        }
    };
    ctx.state
        .lock()
        .unwrap()
        .insert("lo".into(), Managed { method: "loopback", state, configured, ..Default::default() });
}

fn main() {
    let cfg_path: PathBuf = std::env::var_os("MYOS_NETD_CONFIG").map_or("/etc/myos/netd.toml".into(), Into::into);
    let resolv: PathBuf = std::env::var_os("MYOS_NETD_RESOLV").map_or("/etc/resolv.conf".into(), Into::into);
    let cfg = load_config(&cfg_path);
    let ctx = Ctx { state: Arc::new(Mutex::new(BTreeMap::new())), resolv, hostname: hostname() };

    loopback(&ctx);
    {
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("bus".into())
            .spawn(move || myos_services::serve_forever(&["net"], |req| handle(&ctx, req)))
            .expect("spawn bus thread");
    }

    // Start a worker for every configured interface, now and as they appear.
    let mut listed_missing: Vec<String> =
        cfg.interfaces.iter().filter(|(_, m)| **m != Method::Off).map(|(n, _)| n.clone()).collect();
    let started_at = Instant::now();
    let mut warned_missing = false;
    loop {
        for l in link::links() {
            if l.name == "lo" || ctx.state.lock().unwrap().contains_key(&l.name) {
                continue;
            }
            let Some(method) = cfg.method_for(&l.name, l.kind) else { continue };
            listed_missing.retain(|n| *n != l.name);
            let (tx, rx) = mpsc::channel();
            let kind = if method == Method::Dhcp { "dhcp" } else { "static" };
            ctx.state.lock().unwrap().insert(
                l.name.clone(),
                Managed { method: kind, state: "starting".into(), renew: Some(tx), ..Default::default() },
            );
            info!("managing {} ({kind})", l.name);
            let (c, name) = (ctx.clone(), l.name.clone());
            let spawned = std::thread::Builder::new().name(format!("if-{}", l.name)).spawn(move || match method {
                Method::Dhcp => client::run_dhcp(c, name, rx),
                Method::Static { address, prefix, gateway, dns } => client::run_static(c, name, rx, address, prefix, gateway, dns),
                Method::Off => {}
            });
            if let Err(e) = spawned {
                error!("can't start a worker for {}: {e}", l.name);
            }
        }
        if !warned_missing && !listed_missing.is_empty() && started_at.elapsed() > Duration::from_secs(10) {
            warning!("in netd.toml but not present (yet): {}", listed_missing.join(", "));
            warned_missing = true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn list(ctx: &Ctx) -> ListInterfacesReply {
    let mut addrs = link::addresses();
    let routes = link::default_routes();
    let st = ctx.state.lock().unwrap();
    let interfaces = link::links()
        .into_iter()
        .map(|l| {
            let m = st.get(&l.name);
            let (lease_seconds, remaining, server) = match m.and_then(|m| m.lease.as_ref()) {
                Some((lease, at)) => (
                    lease.lease_secs,
                    (lease.lease_secs as u64).saturating_sub(at.elapsed().as_secs()) as u32,
                    lease.server.to_string(),
                ),
                None => (0, 0, String::new()),
            };
            let gateway = m
                .and_then(|m| m.gateway)
                .or_else(|| routes.iter().find(|(i, _)| *i == l.name).map(|(_, g)| *g))
                .map_or(String::new(), |g| g.to_string());
            Interface {
                addresses: addrs
                    .remove(&l.name)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(a, p)| Address { address: a.to_string(), prefix_len: p as u32 })
                    .collect(),
                name: l.name,
                index: l.index,
                mac: l.mac,
                mtu: l.mtu,
                up: l.up,
                carrier: l.carrier,
                method: m.map_or("unmanaged", |m| m.method).into(),
                state: m.map_or(String::new(), |m| m.state.clone()),
                configured: m.is_some_and(|m| m.configured),
                gateway,
                dns: m.map_or(Vec::new(), |m| m.dns.iter().map(|d| d.to_string()).collect()),
                dhcp_server: server,
                lease_seconds,
                lease_remaining_seconds: remaining,
                rx_bytes: l.rx_bytes,
                tx_bytes: l.tx_bytes,
            }
        })
        .collect();
    let managed: Vec<&Managed> = st.values().filter(|m| m.method != "loopback").collect();
    ListInterfacesReply { interfaces, online: !managed.is_empty() && managed.iter().all(|m| m.configured) }
}

fn handle(ctx: &Ctx, req: &Request) -> Result<Vec<u8>, Status> {
    match req.method {
        "ListInterfaces" => {
            let _: ListInterfacesRequest = req.decode()?;
            reply(&list(ctx))
        }
        "Renew" => {
            let r: RenewRequest = req.decode()?;
            if req.caller.uid != 0 {
                return Err(Status::new(Code::PermissionDenied, "only root may renew leases"));
            }
            let st = ctx.state.lock().unwrap();
            match st.get(&r.interface) {
                Some(Managed { renew: Some(tx), method: "dhcp", .. }) => {
                    let _ = tx.send(());
                    info!("{}: renewing on request from pid {}", r.interface, req.caller.pid);
                    reply(&RenewReply {})
                }
                Some(m) => Err(Status::bad_request(format!("{} is {}, not dhcp", r.interface, m.method))),
                None => Err(Status::bad_request(format!("netd doesn't manage {:?}", r.interface))),
            }
        }
        m => Err(Status::unknown_method(m)),
    }
}
