//! Per-interface workers: the DHCP client state machine, static
//! configuration, and the shared state the bus handler reports from.

use crate::dhcp::{self, Lease, Outgoing, Reply};
use crate::link;
use myos_services::{error, info, notice, warning};
use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::os::unix::io::FromRawFd;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What netd knows about an interface it manages.
#[derive(Default)]
pub struct Managed {
    pub method: &'static str,
    pub state: String,
    pub configured: bool,
    pub gateway: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    pub lease: Option<(Lease, Instant)>,
    pub renew: Option<Sender<()>>,
}

#[derive(Clone)]
pub struct Ctx {
    pub state: Arc<Mutex<BTreeMap<String, Managed>>>,
    pub resolv: PathBuf,
    pub hostname: String,
}

impl Ctx {
    fn update(&self, name: &str, f: impl FnOnce(&mut Managed)) {
        if let Some(m) = self.state.lock().unwrap().get_mut(name) {
            f(m);
        }
    }

    fn set_state(&self, name: &str, s: &str) {
        self.update(name, |m| {
            if m.state != s {
                m.state = s.to_string();
            }
        });
    }

    /// resolv.conf from every configured interface's DNS servers.
    pub fn write_resolv(&self) {
        let (mut servers, mut search, mut from) = (Vec::<Ipv4Addr>::new(), None, Vec::new());
        for (name, m) in self.state.lock().unwrap().iter() {
            if !m.configured || m.dns.is_empty() {
                continue;
            }
            from.push(name.clone());
            for d in &m.dns {
                if !servers.contains(d) {
                    servers.push(*d);
                }
            }
            search = search.or(m.domain.clone());
        }
        let mut text = String::from("# Written by netd");
        if !from.is_empty() {
            text.push_str(&format!(" from {}", from.join(", ")));
        }
        text.push_str("; changes here are overwritten.\n");
        if let Some(s) = search {
            text.push_str(&format!("search {s}\n"));
        }
        for s in servers {
            text.push_str(&format!("nameserver {s}\n"));
        }
        if std::fs::read_to_string(&self.resolv).ok().as_deref() == Some(text.as_str()) {
            return;
        }
        // Written in place, not renamed over: Buildroot's /etc/resolv.conf
        // is a symlink to /run/resolv.conf.
        if let Err(e) = std::fs::write(&self.resolv, text) {
            warning!("can't write {}: {e}", self.resolv.display());
        }
    }
}

/// Waits until `deadline`, or until someone asks for a renew (returns true).
fn wait_until(renew: &Receiver<()>, deadline: Instant) -> bool {
    let left = deadline.saturating_duration_since(Instant::now());
    match renew.recv_timeout(left) {
        Ok(()) => {
            while renew.try_recv().is_ok() {}
            true
        }
        Err(RecvTimeoutError::Timeout) => false,
        Err(RecvTimeoutError::Disconnected) => {
            std::thread::sleep(left);
            false
        }
    }
}

/// xorshift64*, seeded from the kernel: for transaction ids and jitter.
struct Rng(u64);

impl Rng {
    fn new() -> Rng {
        let mut b = [0u8; 8];
        let seed = std::fs::File::open("/dev/urandom")
            .and_then(|mut f| io::Read::read_exact(&mut f, &mut b))
            .map(|_| u64::from_ne_bytes(b))
            .unwrap_or_else(|_| myos_services::now_us() as u64 ^ ((std::process::id() as u64) << 32));
        Rng(seed | 1)
    }
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }
    /// `d` give or take a second, as RFC 2131 suggests for retransmissions.
    fn jitter(&mut self, d: Duration) -> Duration {
        (d + Duration::from_millis((self.next() % 2000) as u64)).saturating_sub(Duration::from_secs(1))
    }
}

/// A UDP socket on port 68 that only sees `name`'s traffic and may broadcast.
fn open_socket(name: &str) -> io::Result<UdpSocket> {
    // SAFETY: plain socket calls; the fd is handed to UdpSocket, which owns it.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let sock = UdpSocket::from_raw_fd(fd);
        let one: libc::c_int = 1;
        let int = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let opt = |o, v: *const libc::c_void, len| {
            if libc::setsockopt(fd, libc::SOL_SOCKET, o, v, len) != 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        };
        opt(libc::SO_REUSEADDR, &one as *const _ as *const _, int)?;
        opt(libc::SO_BROADCAST, &one as *const _ as *const _, int)?;
        opt(libc::SO_BINDTODEVICE, name.as_ptr() as *const _, name.len() as libc::socklen_t)?;
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = 68u16.to_be();
        if libc::bind(fd, &addr as *const _ as *const libc::sockaddr, std::mem::size_of_val(&addr) as libc::socklen_t) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sock)
    }
}

const SERVER_PORT: u16 = 67;

struct Dhcp {
    ctx: Ctx,
    name: String,
    renew: Receiver<()>,
    rng: Rng,
    mac: [u8; 6],
    sock: Option<UdpSocket>,
    /// The lease currently applied to the interface.
    applied: Option<Lease>,
}

enum Wait {
    Got(Reply, Ipv4Addr),
    Nothing,
}

impl Dhcp {
    fn send(&self, out: &Outgoing, to: Ipv4Addr) -> io::Result<()> {
        let sock = self.sock.as_ref().ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))?;
        sock.send_to(&dhcp::build(out), SocketAddrV4::new(to, SERVER_PORT)).map(|_| ())
    }

    /// Waits up to `d` for a reply to `xid` of one of `kinds`.
    fn receive(&self, xid: u32, kinds: &[u8], d: Duration) -> Wait {
        let Some(sock) = &self.sock else { return Wait::Nothing };
        let deadline = Instant::now() + d;
        let mut buf = [0u8; 1500];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || sock.set_read_timeout(Some(left)).is_err() {
                return Wait::Nothing;
            }
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if let Some(r) = dhcp::parse(&buf[..n], xid, &self.mac) {
                        if kinds.contains(&r.kind) {
                            let from = match from {
                                std::net::SocketAddr::V4(a) => *a.ip(),
                                _ => Ipv4Addr::UNSPECIFIED,
                            };
                            return Wait::Got(r, from);
                        }
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
                Err(_) => return Wait::Nothing,
            }
        }
    }

    fn outgoing(&self, kind: u8, xid: u32, started: Instant) -> Outgoing<'_> {
        Outgoing {
            kind,
            xid,
            mac: self.mac,
            secs: started.elapsed().as_secs().min(u16::MAX as u64) as u16,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            requested: None,
            server_id: None,
            hostname: &self.ctx.hostname,
        }
    }

    /// Up, with carrier, with a socket. Loops until it is.
    fn prepare(&mut self) {
        let mut said = String::new();
        loop {
            let problem = match link::link(&self.name) {
                None => "waiting for link: interface not present".to_string(),
                Some(l) if !l.up => match link::set_up(&self.name) {
                    Ok(()) => continue,
                    Err(e) => format!("failed: can't bring it up: {e}"),
                },
                Some(l) if !l.carrier => "waiting for link".to_string(),
                Some(l) => {
                    self.mac = link::mac_bytes(&l.mac);
                    if self.sock.is_some() {
                        return;
                    }
                    match open_socket(&self.name) {
                        Ok(s) => {
                            self.sock = Some(s);
                            return;
                        }
                        Err(e) => format!("failed: can't open the DHCP socket: {e}"),
                    }
                }
            };
            if problem != said {
                notice!("{}: {problem}", self.name);
                self.ctx.set_state(&self.name, &problem);
                said = problem;
            }
            wait_until(&self.renew, Instant::now() + Duration::from_secs(1));
        }
    }

    /// DISCOVER until someone OFFERs, then REQUEST it. Returns the ACK.
    fn acquire(&mut self) -> (Reply, Ipv4Addr) {
        loop {
            self.prepare();
            self.ctx.set_state(&self.name, "discovering");
            let started = Instant::now();
            let xid = self.rng.next();
            let mut delay = Duration::from_secs(4);
            let offer = loop {
                if let Err(e) = self.send(&self.outgoing(dhcp::DISCOVER, xid, started), Ipv4Addr::BROADCAST) {
                    warning!("{}: sending DHCPDISCOVER: {e}", self.name);
                    self.sock = None;
                    break None;
                }
                let d = self.rng.jitter(delay);
                if let Wait::Got(r, from) = self.receive(xid, &[dhcp::OFFER], d) {
                    break Some((r, from));
                }
                delay = (delay * 2).min(Duration::from_secs(32));
                // Link went away? Start over once it's back.
                if !link::link(&self.name).is_some_and(|l| l.carrier) {
                    break None;
                }
            };
            let Some((offer, from)) = offer else { continue };
            let server = offer.server_id.unwrap_or(from);

            self.ctx.set_state(&self.name, "requesting");
            for _ in 0..4 {
                let mut out = self.outgoing(dhcp::REQUEST, xid, started);
                out.requested = Some(offer.yiaddr);
                out.server_id = Some(server);
                if self.send(&out, Ipv4Addr::BROADCAST).is_err() {
                    break;
                }
                match self.receive(xid, &[dhcp::ACK, dhcp::NAK], Duration::from_secs(4)) {
                    Wait::Got(r, from) if r.kind == dhcp::ACK => return (r, from),
                    Wait::Got(_, _) => {
                        notice!("{}: {server} declined our request for {}", self.name, offer.yiaddr);
                        break;
                    }
                    Wait::Nothing => {}
                }
            }
        }
    }

    fn apply(&mut self, lease: Lease) {
        let have = link::addresses().remove(&self.name).unwrap_or_default();
        if have != [(lease.address, lease.prefix)] {
            if let Err(e) = link::set_addr(&self.name, lease.address, lease.prefix) {
                error!("{}: can't set address {}/{}: {e}", self.name, lease.address, lease.prefix);
            }
        }
        if let Err(e) = link::set_default_route(&self.name, lease.router) {
            error!("{}: can't set the default route via {:?}: {e}", self.name, lease.router);
        }
        if self.applied.as_ref() != Some(&lease) {
            info!(
                "{}: {}/{} from {}, gateway {}, dns {}, lease {}",
                self.name,
                lease.address,
                lease.prefix,
                lease.server,
                lease.router.map_or("none".into(), |r| r.to_string()),
                if lease.dns.is_empty() { "none".into() } else { lease.dns.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(" ") },
                myos_services::human_duration(lease.lease_secs as u64)
            );
        }
        let l = lease.clone();
        self.ctx.update(&self.name, |m| {
            m.configured = true;
            m.gateway = l.router;
            m.dns = l.dns.clone();
            m.domain = l.domain.clone();
            m.lease = Some((l, Instant::now()));
            m.state = "bound".into();
        });
        self.applied = Some(lease);
        self.ctx.write_resolv();
    }

    fn deconfigure(&mut self, why: &str) {
        warning!("{}: {why}; removing {}", self.name, self.applied.as_ref().map_or("its address".into(), |l| l.address.to_string()));
        let _ = link::clear_addr(&self.name);
        self.applied = None;
        self.ctx.update(&self.name, |m| {
            m.configured = false;
            m.gateway = None;
            m.dns.clear();
            m.domain = None;
            m.lease = None;
        });
        self.ctx.write_resolv();
    }

    /// Bound: waits for T1 or a renew request, checking the link every 2 s.
    /// If the interface was brought down or lost its carrier (which also
    /// drops the default route), returns as soon as it's back, so the lease
    /// is confirmed with the server and the route restored.
    fn watch_until(&mut self, t1: Instant) {
        let mut lost = false;
        loop {
            if wait_until(&self.renew, t1.min(Instant::now() + Duration::from_secs(2))) || Instant::now() >= t1 {
                return;
            }
            match link::link(&self.name) {
                Some(l) if !l.up => {
                    if !lost {
                        notice!("{}: was brought down; bringing it back up", self.name);
                        self.ctx.set_state(&self.name, "waiting for link");
                    }
                    let _ = link::set_up(&self.name);
                    lost = true;
                }
                Some(l) if !l.carrier => {
                    if !lost {
                        notice!("{}: lost carrier", self.name);
                        self.ctx.set_state(&self.name, "waiting for link");
                    }
                    lost = true;
                }
                Some(_) if lost => {
                    info!("{}: link is back; confirming the lease", self.name);
                    return;
                }
                Some(_) => {}
                None => lost = true,
            }
        }
    }

    /// Holds the lease: renews at T1 (unicast to the server), rebinds at T2
    /// (broadcast), gives up at expiry. Returns when the lease is lost.
    fn hold(&mut self, mut lease: Lease) {
        let mut acquired = Instant::now();
        loop {
            let at = |s: u32| acquired + Duration::from_secs(s as u64);
            let (t1, t2, end) = (at(lease.t1), at(lease.t2), at(lease.lease_secs));
            self.ctx.set_state(&self.name, "bound");
            self.watch_until(t1);

            let started = Instant::now();
            let mut renewed = None;
            while Instant::now() < end {
                let rebinding = Instant::now() >= t2;
                self.ctx.set_state(&self.name, if rebinding { "rebinding" } else { "renewing" });
                if self.sock.is_none() {
                    self.sock = open_socket(&self.name).ok();
                }
                let xid = self.rng.next();
                let mut out = self.outgoing(dhcp::REQUEST, xid, started);
                out.ciaddr = lease.address;
                let to = if rebinding { Ipv4Addr::BROADCAST } else { lease.server };
                if self.send(&out, to).is_ok() {
                    match self.receive(xid, &[dhcp::ACK, dhcp::NAK], Duration::from_secs(4)) {
                        Wait::Got(r, from) if r.kind == dhcp::ACK => {
                            renewed = Some(r.lease(from));
                            break;
                        }
                        Wait::Got(_, _) => {
                            self.deconfigure("the DHCP server refused to renew the lease");
                            return;
                        }
                        Wait::Nothing => {}
                    }
                }
                // RFC 2131: wait half the time left (to T2, or to expiry), at least 10 s here.
                let now = Instant::now();
                let target = if now >= t2 { end } else { t2 };
                let pause = (target.saturating_duration_since(now) / 2).max(Duration::from_secs(10));
                wait_until(&self.renew, (now + pause).min(end));
            }
            match renewed {
                Some(new) => {
                    lease = new;
                    acquired = Instant::now();
                    self.apply(lease.clone());
                }
                None => {
                    self.deconfigure("the DHCP lease expired");
                    return;
                }
            }
        }
    }
}

pub fn run_dhcp(ctx: Ctx, name: String, renew: Receiver<()>) {
    let mut c = Dhcp { ctx, name, renew, rng: Rng::new(), mac: [0; 6], sock: None, applied: None };
    loop {
        let (ack, from) = c.acquire();
        let lease = ack.lease(from);
        c.apply(lease.clone());
        c.hold(lease);
    }
}

pub fn run_static(ctx: Ctx, name: String, renew: Receiver<()>, address: Ipv4Addr, prefix: u8, gateway: Option<Ipv4Addr>, dns: Vec<Ipv4Addr>) {
    let mut announced = false;
    loop {
        let result = (|| -> io::Result<()> {
            if link::link(&name).is_none() {
                return Err(io::Error::new(io::ErrorKind::NotFound, "interface not present"));
            }
            link::set_up(&name)?;
            if link::addresses().remove(&name).unwrap_or_default() != [(address, prefix)] {
                link::set_addr(&name, address, prefix)?;
            }
            if gateway.is_some() {
                link::set_default_route(&name, gateway)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                if !announced {
                    info!("{name}: {address}/{prefix} (static), gateway {}", gateway.map_or("none".into(), |g| g.to_string()));
                    announced = true;
                }
                let dns = dns.clone();
                ctx.update(&name, |m| {
                    m.configured = true;
                    m.state = "configured".into();
                    m.gateway = gateway;
                    m.dns = dns;
                });
                ctx.write_resolv();
            }
            Err(e) => {
                let s = if e.kind() == io::ErrorKind::NotFound { format!("waiting for link: {e}") } else { format!("failed: {e}") };
                ctx.update(&name, |m| {
                    m.configured = false;
                    m.state = s;
                });
                announced = false;
            }
        }
        // Check again now and then, in case the address was removed by hand
        // or the interface came back.
        wait_until(&renew, Instant::now() + Duration::from_secs(if announced { 30 } else { 2 }));
    }
}
