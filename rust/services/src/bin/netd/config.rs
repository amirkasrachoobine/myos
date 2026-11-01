//! /etc/myos/netd.toml: one table per interface.
//!
//! ```toml
//! [eth0]
//! method = "dhcp"
//!
//! [eth1]
//! method = "static"
//! address = "192.168.1.10/24"
//! gateway = "192.168.1.1"       # optional
//! dns = ["192.168.1.1"]         # optional
//!
//! [eth2]
//! method = "off"                # leave it alone
//! ```
//!
//! Interfaces not listed are left alone. Without the file, netd runs DHCP
//! on every Ethernet interface. lo is always brought up as 127.0.0.1/8.

// The TOML subset parser that init uses for service files.
#[path = "../../../../init/src/toml.rs"]
#[allow(dead_code)]
mod toml;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use toml::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    Dhcp,
    Static { address: Ipv4Addr, prefix: u8, gateway: Option<Ipv4Addr>, dns: Vec<Ipv4Addr> },
    Off,
}

#[derive(Debug, Default, PartialEq)]
pub struct Config {
    pub interfaces: BTreeMap<String, Method>,
    /// No config file: DHCP on every Ethernet interface.
    pub auto: bool,
}

impl Config {
    pub fn auto() -> Config {
        Config { interfaces: BTreeMap::new(), auto: true }
    }

    /// What to do with interface `name` of ARPHRD type `kind` (1 = Ethernet).
    pub fn method_for(&self, name: &str, kind: u32) -> Option<Method> {
        match self.interfaces.get(name) {
            Some(Method::Off) => None,
            Some(m) => Some(m.clone()),
            None if self.auto && kind == 1 => Some(Method::Dhcp),
            None => None,
        }
    }
}

pub fn parse_cidr(s: &str) -> Option<(Ipv4Addr, u8)> {
    let (a, p) = s.split_once('/')?;
    let prefix: u8 = p.parse().ok()?;
    (prefix <= 32).then_some(())?;
    Some((a.parse().ok()?, prefix))
}

pub fn parse(text: &str) -> Result<Config, String> {
    let doc = toml::parse(text)?;
    if let Some(k) = doc.root.keys().next() {
        return Err(format!("`{k}` must be inside an interface table such as [eth0]"));
    }
    let mut cfg = Config::default();
    for (name, t) in &doc.tables {
        let err = |m: String| format!("[{name}]: {m}");
        for k in t.keys() {
            if !["method", "address", "gateway", "dns"].contains(&k.as_str()) {
                return Err(err(format!("unknown key `{k}` (known keys: method, address, gateway, dns)")));
            }
        }
        let string = |k: &str| match t.get(k) {
            None => Ok(None),
            Some(Value::Str(s)) => Ok(Some(s.clone())),
            Some(v) => Err(err(format!("`{k}` must be a string, not {}", v.type_name()))),
        };
        let ip = |k: &str| -> Result<Option<Ipv4Addr>, String> {
            string(k)?.map(|s| s.parse().map_err(|_| err(format!("`{k}`: `{s}` is not an IPv4 address")))).transpose()
        };
        let method = match string("method")?.as_deref() {
            Some("dhcp") => Method::Dhcp,
            Some("off") => Method::Off,
            Some("static") => {
                let a = string("address")?.ok_or_else(|| err("method \"static\" needs `address = \"A.B.C.D/N\"`".into()))?;
                let (address, prefix) = parse_cidr(&a).ok_or_else(|| err(format!("`address`: `{a}` is not A.B.C.D/N")))?;
                let dns = match t.get("dns") {
                    None => Vec::new(),
                    Some(Value::Array(items)) => items
                        .iter()
                        .map(|v| match v {
                            Value::Str(s) => s.parse().map_err(|_| err(format!("`dns`: `{s}` is not an IPv4 address"))),
                            _ => Err(err("`dns` must be a list of strings".into())),
                        })
                        .collect::<Result<_, _>>()?,
                    Some(_) => return Err(err("`dns` must be a list of strings".into())),
                };
                Method::Static { address, prefix, gateway: ip("gateway")?, dns }
            }
            Some(m) => return Err(err(format!("method must be \"dhcp\", \"static\" or \"off\", not \"{m}\""))),
            None => return Err(err("missing `method`".into())),
        };
        let static_keys = ["address", "gateway", "dns"].iter().any(|k| t.contains_key(*k));
        if static_keys && matches!(method, Method::Dhcp | Method::Off) {
            return Err(err("address, gateway and dns only apply to method = \"static\"".into()));
        }
        cfg.interfaces.insert(name.clone(), method);
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good() {
        let c = parse(
            "[eth0]\nmethod = \"dhcp\"\n\n[eth1]\nmethod = \"static\"\naddress = \"192.168.1.10/24\"\ngateway = \"192.168.1.1\"\ndns = [\"1.1.1.1\", \"9.9.9.9\"]\n[eth2]\nmethod = 'off'\n",
        )
        .unwrap();
        assert_eq!(c.method_for("eth0", 1), Some(Method::Dhcp));
        assert_eq!(
            c.method_for("eth1", 1),
            Some(Method::Static {
                address: "192.168.1.10".parse().unwrap(),
                prefix: 24,
                gateway: Some("192.168.1.1".parse().unwrap()),
                dns: vec!["1.1.1.1".parse().unwrap(), "9.9.9.9".parse().unwrap()],
            })
        );
        assert_eq!(c.method_for("eth2", 1), None);
        assert_eq!(c.method_for("eth3", 1), None);
        assert_eq!(Config::auto().method_for("eth3", 1), Some(Method::Dhcp));
        assert_eq!(Config::auto().method_for("sit0", 776), None);
    }

    #[test]
    fn bad() {
        for (text, why) in [
            ("method = \"dhcp\"", "inside an interface table"),
            ("[eth0]\nmethod = \"magic\"", "not \"magic\""),
            ("[eth0]\n", "missing `method`"),
            ("[eth0]\nmethod = \"static\"", "needs `address"),
            ("[eth0]\nmethod = \"static\"\naddress = \"10.0.0.1\"", "is not A.B.C.D/N"),
            ("[eth0]\nmethod = \"static\"\naddress = \"10.0.0.1/24\"\ngateway = \"x\"", "not an IPv4 address"),
            ("[eth0]\nmethod = \"dhcp\"\naddress = \"10.0.0.1/24\"", "only apply"),
            ("[eth0]\nmethod = \"dhcp\"\nmtu = 1400", "unknown key `mtu`"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(why), "{text:?}: {e}");
        }
    }
}
