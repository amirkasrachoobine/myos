//! Interfaces, addresses and routes, through sysfs and the classic
//! interface ioctls (one IPv4 address per interface, which is all netd sets).

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::io;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Default)]
pub struct Link {
    pub name: String,
    pub index: u32,
    pub mac: String,
    pub mtu: u32,
    pub up: bool,
    pub carrier: bool,
    /// ARPHRD_*: 1 Ethernet, 772 loopback.
    pub kind: u32,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

fn sys(name: &str, file: &str) -> Option<String> {
    fs::read_to_string(format!("/sys/class/net/{name}/{file}")).ok().map(|s| s.trim().to_string())
}

fn num(name: &str, file: &str) -> u64 {
    sys(name, file).and_then(|s| s.parse().ok()).unwrap_or(0)
}

pub fn link(name: &str) -> Option<Link> {
    let index = sys(name, "ifindex")?.parse().ok()?;
    let flags = sys(name, "flags").and_then(|f| u32::from_str_radix(f.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
    Some(Link {
        name: name.to_string(),
        index,
        mac: sys(name, "address").unwrap_or_default(),
        mtu: num(name, "mtu") as u32,
        up: flags & libc::IFF_UP as u32 != 0,
        // Reading carrier fails with EINVAL while the interface is down.
        carrier: sys(name, "carrier").as_deref() == Some("1"),
        kind: num(name, "type") as u32,
        rx_bytes: num(name, "statistics/rx_bytes"),
        tx_bytes: num(name, "statistics/tx_bytes"),
    })
}

pub fn links() -> Vec<Link> {
    let Ok(dir) = fs::read_dir("/sys/class/net") else { return Vec::new() };
    let mut v: Vec<Link> = dir.filter_map(|e| link(e.ok()?.file_name().to_str()?)).collect();
    v.sort_by_key(|l| l.index);
    v
}

pub fn mac_bytes(mac: &str) -> [u8; 6] {
    let mut out = [0u8; 6];
    for (i, part) in mac.split(':').take(6).enumerate() {
        out[i] = u8::from_str_radix(part, 16).unwrap_or(0);
    }
    out
}

pub fn prefix_to_mask(prefix: u8) -> Ipv4Addr {
    let p = prefix.min(32) as u32;
    Ipv4Addr::from(if p == 0 { 0 } else { u32::MAX << (32 - p) })
}

pub fn mask_to_prefix(mask: Ipv4Addr) -> u8 {
    u32::from(mask).leading_ones() as u8
}

// ---- ioctls --------------------------------------------------------------

/// struct ifreq: the name, then a 24-byte union (sockaddr, flags, ...).
#[repr(C)]
struct IfReq {
    name: [u8; libc::IFNAMSIZ],
    data: [u8; 24],
}

impl IfReq {
    fn new(name: &str) -> io::Result<IfReq> {
        if name.is_empty() || name.len() >= libc::IFNAMSIZ {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("bad interface name {name:?}")));
        }
        let mut r = IfReq { name: [0; libc::IFNAMSIZ], data: [0; 24] };
        r.name[..name.len()].copy_from_slice(name.as_bytes());
        Ok(r)
    }

    fn with_addr(name: &str, a: Ipv4Addr) -> io::Result<IfReq> {
        let mut r = IfReq::new(name)?;
        r.data[..16].copy_from_slice(&sockaddr_in(a));
        Ok(r)
    }
}

/// struct sockaddr_in as bytes (port 0).
fn sockaddr_in(a: Ipv4Addr) -> [u8; 16] {
    let mut s = [0u8; 16];
    s[..2].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
    s[4..8].copy_from_slice(&a.octets());
    s
}

fn sockaddr(a: Ipv4Addr) -> libc::sockaddr {
    // SAFETY: sockaddr is 16 bytes of plain data, the same size as sockaddr_in.
    unsafe { std::mem::transmute::<[u8; 16], libc::sockaddr>(sockaddr_in(a)) }
}

struct Sock(i32);

impl Sock {
    fn new() -> io::Result<Sock> {
        // SAFETY: plain socket(2).
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Sock(fd))
        }
    }

    fn ioctl<T>(&self, req: libc::c_ulong, arg: &mut T) -> io::Result<()> {
        // SAFETY: arg is the struct this request expects (ifreq or rtentry).
        if unsafe { libc::ioctl(self.0, req as _, arg as *mut T) } < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for Sock {
    fn drop(&mut self) {
        // SAFETY: we own the fd.
        unsafe { libc::close(self.0) };
    }
}

pub fn set_up(name: &str) -> io::Result<()> {
    let s = Sock::new()?;
    let mut r = IfReq::new(name)?;
    s.ioctl(libc::SIOCGIFFLAGS, &mut r)?;
    let flags = i16::from_ne_bytes([r.data[0], r.data[1]]) | libc::IFF_UP as i16;
    r.data[..2].copy_from_slice(&flags.to_ne_bytes());
    s.ioctl(libc::SIOCSIFFLAGS, &mut r)
}

/// Replaces the interface's IPv4 address. The kernel adds the subnet route.
pub fn set_addr(name: &str, addr: Ipv4Addr, prefix: u8) -> io::Result<()> {
    let s = Sock::new()?;
    s.ioctl(libc::SIOCSIFADDR, &mut IfReq::with_addr(name, addr)?)?;
    let mask = prefix_to_mask(prefix);
    s.ioctl(libc::SIOCSIFNETMASK, &mut IfReq::with_addr(name, mask)?)?;
    if prefix < 31 {
        let bcast = Ipv4Addr::from(u32::from(addr) | !u32::from(mask));
        s.ioctl(libc::SIOCSIFBRDADDR, &mut IfReq::with_addr(name, bcast)?)?;
    }
    Ok(())
}

/// Removes the interface's IPv4 address, and with it its routes.
pub fn clear_addr(name: &str) -> io::Result<()> {
    let s = Sock::new()?;
    match s.ioctl(libc::SIOCSIFADDR, &mut IfReq::with_addr(name, Ipv4Addr::UNSPECIFIED)?) {
        Err(e) if e.raw_os_error() == Some(libc::EADDRNOTAVAIL) => Ok(()),
        r => r,
    }
}

/// IPv4 addresses per interface name.
pub fn addresses() -> HashMap<String, Vec<(Ipv4Addr, u8)>> {
    let mut out: HashMap<String, Vec<(Ipv4Addr, u8)>> = HashMap::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a list we walk and then free.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut p = head;
    while !p.is_null() {
        // SAFETY: p is a node of the list getifaddrs returned.
        let ifa = unsafe { &*p };
        p = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || unsafe { (*ifa.ifa_addr).sa_family } as i32 != libc::AF_INET {
            continue;
        }
        let v4 = |sa: *const libc::sockaddr| {
            if sa.is_null() {
                return Ipv4Addr::UNSPECIFIED;
            }
            // SAFETY: AF_INET entries carry sockaddr_in.
            let sin = unsafe { &*(sa as *const libc::sockaddr_in) };
            Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))
        };
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned();
        out.entry(name).or_default().push((v4(ifa.ifa_addr), mask_to_prefix(v4(ifa.ifa_netmask))));
    }
    // SAFETY: freeing what getifaddrs gave us.
    unsafe { libc::freeifaddrs(head) };
    out
}

/// Default routes as (interface, gateway), from /proc/net/route.
pub fn default_routes() -> Vec<(String, Ipv4Addr)> {
    parse_routes(&fs::read_to_string("/proc/net/route").unwrap_or_default())
}

pub fn parse_routes(text: &str) -> Vec<(String, Ipv4Addr)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let hex = |i: usize| f.get(i).and_then(|v| u32::from_str_radix(v, 16).ok());
            // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
            (hex(1)? == 0 && hex(7)? == 0).then_some(())?;
            Some((f[0].to_string(), Ipv4Addr::from(u32::from_be(hex(2)?))))
        })
        .collect()
}

fn route_entry(name: &std::ffi::CString, gw: Ipv4Addr) -> libc::rtentry {
    // SAFETY: rtentry is plain data; zero is a valid "unset" for every field.
    let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };
    rt.rt_dst = sockaddr(Ipv4Addr::UNSPECIFIED);
    rt.rt_genmask = sockaddr(Ipv4Addr::UNSPECIFIED);
    rt.rt_gateway = sockaddr(gw);
    rt.rt_flags = libc::RTF_UP | if gw.is_unspecified() { 0 } else { libc::RTF_GATEWAY };
    rt.rt_dev = name.as_ptr() as *mut libc::c_char;
    rt
}

/// Makes `gw` via `name` the default route, removing any other default
/// route through `name`. With `gw` None, only removes.
pub fn set_default_route(name: &str, gw: Option<Ipv4Addr>) -> io::Result<()> {
    let current: Vec<Ipv4Addr> = default_routes().into_iter().filter(|(i, _)| i == name).map(|(_, g)| g).collect();
    if gw.is_some_and(|g| current == [g]) {
        return Ok(());
    }
    let s = Sock::new()?;
    let cname = std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    for old in current {
        let mut rt = route_entry(&cname, old);
        let _ = s.ioctl(libc::SIOCDELRT, &mut rt);
    }
    if let Some(g) = gw {
        let mut rt = route_entry(&cname, g);
        s.ioctl(libc::SIOCADDRT, &mut rt)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        assert_eq!(prefix_to_mask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_to_mask(0), Ipv4Addr::UNSPECIFIED);
        assert_eq!(prefix_to_mask(32), Ipv4Addr::BROADCAST);
        assert_eq!(mask_to_prefix(Ipv4Addr::new(255, 255, 240, 0)), 20);
        assert_eq!(mac_bytes("52:54:00:12:34:56"), [0x52, 0x54, 0, 0x12, 0x34, 0x56]);
    }

    #[test]
    fn routes() {
        let t = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t0202000A\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
                 eth0\t0002000A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(parse_routes(t), vec![("eth0".to_string(), Ipv4Addr::new(10, 0, 2, 2))]);
    }
}
