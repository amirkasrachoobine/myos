//! DHCPv4 messages (RFC 2131 / 2132): building requests, reading replies.

use std::net::Ipv4Addr;

pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;
pub const RELEASE: u8 = 7;

const MAGIC: [u8; 4] = [99, 130, 83, 99];
/// Ask the server to broadcast its replies: until we have an address, the
/// kernel drops unicast IP sent to an address we don't have yet.
const FLAG_BROADCAST: u16 = 0x8000;

/// What we send.
pub struct Outgoing<'a> {
    pub kind: u8,
    pub xid: u32,
    pub mac: [u8; 6],
    pub secs: u16,
    /// Our current address, when renewing or rebinding.
    pub ciaddr: Ipv4Addr,
    /// Option 50, when selecting an offer.
    pub requested: Option<Ipv4Addr>,
    /// Option 54, when selecting an offer.
    pub server_id: Option<Ipv4Addr>,
    pub hostname: &'a str,
}

pub fn build(m: &Outgoing) -> Vec<u8> {
    let mut p = vec![0u8; 236];
    p[0] = 1; // BOOTREQUEST
    p[1] = 1; // Ethernet
    p[2] = 6;
    p[4..8].copy_from_slice(&m.xid.to_be_bytes());
    p[8..10].copy_from_slice(&m.secs.to_be_bytes());
    if m.ciaddr.is_unspecified() {
        p[10..12].copy_from_slice(&FLAG_BROADCAST.to_be_bytes());
    }
    p[12..16].copy_from_slice(&m.ciaddr.octets());
    p[28..34].copy_from_slice(&m.mac);
    p.extend_from_slice(&MAGIC);

    let mut opt = |code: u8, data: &[u8]| {
        p.push(code);
        p.push(data.len() as u8);
        p.extend_from_slice(data);
    };
    opt(53, &[m.kind]);
    let mut client_id = vec![1u8];
    client_id.extend_from_slice(&m.mac);
    opt(61, &client_id);
    if let Some(r) = m.requested {
        opt(50, &r.octets());
    }
    if let Some(s) = m.server_id {
        opt(54, &s.octets());
    }
    if m.kind != RELEASE {
        let h = m.hostname.as_bytes();
        if !h.is_empty() {
            opt(12, &h[..h.len().min(63)]);
        }
        // subnet mask, router, DNS, domain name, broadcast, lease, T1, T2
        opt(55, &[1, 3, 6, 15, 28, 51, 58, 59]);
        opt(57, &1500u16.to_be_bytes());
    }
    p.push(255);
    // Some servers ignore BOOTP messages shorter than 300 bytes.
    if p.len() < 300 {
        p.resize(300, 0);
    }
    p
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub kind: u8,
    pub yiaddr: Ipv4Addr,
    pub server_id: Option<Ipv4Addr>,
    pub prefix: Option<u8>,
    pub routers: Vec<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    pub lease_secs: Option<u32>,
    pub t1: Option<u32>,
    pub t2: Option<u32>,
}

impl Default for Reply {
    fn default() -> Reply {
        Reply {
            kind: 0,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            server_id: None,
            prefix: None,
            routers: Vec::new(),
            dns: Vec::new(),
            domain: None,
            lease_secs: None,
            t1: None,
            t2: None,
        }
    }
}

fn addrs(data: &[u8]) -> Vec<Ipv4Addr> {
    data.chunks_exact(4).map(|c| Ipv4Addr::new(c[0], c[1], c[2], c[3])).collect()
}

fn be32(data: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(..4)?.try_into().ok()?))
}

/// Reads a server's reply; None if it's not a DHCP reply to `xid` for `mac`.
pub fn parse(p: &[u8], xid: u32, mac: &[u8; 6]) -> Option<Reply> {
    if p.len() < 240 || p[0] != 2 || p[4..8] != xid.to_be_bytes() || &p[28..34] != mac || p[236..240] != MAGIC {
        return None;
    }
    let mut r = Reply { yiaddr: Ipv4Addr::new(p[16], p[17], p[18], p[19]), ..Default::default() };
    let mut i = 240;
    while i < p.len() {
        let code = p[i];
        if code == 0 {
            i += 1;
            continue;
        }
        if code == 255 {
            break;
        }
        let len = *p.get(i + 1)? as usize;
        let data = p.get(i + 2..i + 2 + len)?;
        match code {
            53 => r.kind = *data.first()?,
            54 => r.server_id = addrs(data).first().copied(),
            1 => r.prefix = addrs(data).first().map(|m| u32::from(*m).leading_ones() as u8),
            3 => r.routers = addrs(data),
            6 => r.dns = addrs(data),
            15 => r.domain = Some(String::from_utf8_lossy(data).trim_end_matches('\0').to_string()).filter(|d| !d.is_empty()),
            51 => r.lease_secs = be32(data),
            58 => r.t1 = be32(data),
            59 => r.t2 = be32(data),
            _ => {}
        }
        i += 2 + len;
    }
    (r.kind != 0).then_some(r)
}

/// A lease, as applied to an interface.
#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    pub server: Ipv4Addr,
    pub lease_secs: u32,
    pub t1: u32,
    pub t2: u32,
}

impl Reply {
    /// The lease an ACK grants. `from` is where the reply came from, used if
    /// it has no server identifier.
    pub fn lease(&self, from: Ipv4Addr) -> Lease {
        // Infinite (0xffffffff) or missing: renew daily anyway.
        let lease_secs = self.lease_secs.unwrap_or(86_400).min(u32::MAX - 1).max(10);
        let t1 = self.t1.filter(|t| *t < lease_secs).unwrap_or(lease_secs / 2);
        let t2 = self.t2.filter(|t| *t < lease_secs && *t > t1).unwrap_or(lease_secs / 8 * 7);
        Lease {
            address: self.yiaddr,
            prefix: self.prefix.unwrap_or_else(|| default_prefix(self.yiaddr)),
            router: self.routers.first().copied(),
            dns: self.dns.clone(),
            domain: self.domain.clone(),
            server: self.server_id.unwrap_or(from),
            lease_secs,
            t1,
            t2: t2.max(t1),
        }
    }
}

/// Classful default, for servers that don't send a subnet mask.
fn default_prefix(a: Ipv4Addr) -> u8 {
    match a.octets()[0] {
        0..=127 => 8,
        128..=191 => 16,
        _ => 24,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];

    fn server_reply(kind: u8, xid: u32, opts: &[(u8, &[u8])]) -> Vec<u8> {
        let mut p = vec![0u8; 236];
        p[0] = 2;
        p[1] = 1;
        p[2] = 6;
        p[4..8].copy_from_slice(&xid.to_be_bytes());
        p[16..20].copy_from_slice(&[10, 0, 2, 15]);
        p[28..34].copy_from_slice(&MAC);
        p.extend_from_slice(&MAGIC);
        p.extend_from_slice(&[53, 1, kind]);
        for (c, d) in opts {
            p.push(*c);
            p.push(d.len() as u8);
            p.extend_from_slice(d);
        }
        p.push(255);
        p
    }

    #[test]
    fn discover_layout() {
        let p = build(&Outgoing {
            kind: DISCOVER,
            xid: 0xdeadbeef,
            mac: MAC,
            secs: 3,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            requested: None,
            server_id: None,
            hostname: "myos",
        });
        assert_eq!(p.len(), 300);
        assert_eq!(&p[..4], &[1, 1, 6, 0]);
        assert_eq!(&p[4..8], &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(&p[8..12], &[0, 3, 0x80, 0]);
        assert_eq!(&p[28..34], &MAC);
        assert_eq!(&p[236..243], &[99, 130, 83, 99, 53, 1, DISCOVER]);
        let has = |needle: &[u8]| p.windows(needle.len()).any(|w| w == needle);
        assert!(has(&[61, 7, 1, 0x52, 0x54, 0, 0x12, 0x34, 0x56]));
        assert!(has(&[12, 4, b'm', b'y', b'o', b's']));
    }

    #[test]
    fn renew_is_unicast_shaped() {
        let p = build(&Outgoing {
            kind: REQUEST,
            xid: 1,
            mac: MAC,
            secs: 0,
            ciaddr: Ipv4Addr::new(10, 0, 2, 15),
            requested: None,
            server_id: None,
            hostname: "",
        });
        assert_eq!(&p[10..16], &[0, 0, 10, 0, 2, 15]);
        assert!(!p.windows(2).any(|w| w == [50, 4]));
    }

    #[test]
    fn ack() {
        let p = server_reply(ACK, 7, &[
            (54, &[10, 0, 2, 2]),
            (1, &[255, 255, 255, 0]),
            (3, &[10, 0, 2, 2]),
            (6, &[10, 0, 2, 3, 1, 1, 1, 1]),
            (15, b"example.org\0"),
            (51, &86400u32.to_be_bytes()),
        ]);
        let r = parse(&p, 7, &MAC).unwrap();
        assert_eq!(r.kind, ACK);
        let l = r.lease(Ipv4Addr::new(10, 0, 2, 2));
        assert_eq!(l.address, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(l.prefix, 24);
        assert_eq!(l.router, Some(Ipv4Addr::new(10, 0, 2, 2)));
        assert_eq!(l.dns, vec![Ipv4Addr::new(10, 0, 2, 3), Ipv4Addr::new(1, 1, 1, 1)]);
        assert_eq!(l.domain.as_deref(), Some("example.org"));
        assert_eq!((l.lease_secs, l.t1, l.t2), (86400, 43200, 75600));
    }

    #[test]
    fn rejects_others() {
        let p = server_reply(OFFER, 7, &[]);
        assert!(parse(&p, 8, &MAC).is_none(), "wrong xid");
        assert!(parse(&p, 7, &[1, 2, 3, 4, 5, 6]).is_none(), "wrong mac");
        assert!(parse(&p[..200], 7, &MAC).is_none(), "short");
        let mut req = p.clone();
        req[0] = 1;
        assert!(parse(&req, 7, &MAC).is_none(), "a request, not a reply");
        // truncated option
        let mut bad = server_reply(OFFER, 7, &[]);
        bad.pop();
        bad.extend_from_slice(&[6, 8, 1, 2]);
        assert!(parse(&bad, 7, &MAC).is_none());
    }

    #[test]
    fn odd_lease_times() {
        let r = Reply { kind: ACK, yiaddr: Ipv4Addr::new(192, 168, 0, 9), lease_secs: Some(u32::MAX), t1: Some(5), t2: Some(3), ..Default::default() };
        let l = r.lease(Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(l.prefix, 24);
        assert_eq!(l.server, Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(l.t1, 5);
        assert!(l.t2 > l.t1 && l.t2 < l.lease_secs);
    }
}
