//! Parsing what arrives on /dev/log and /dev/kmsg.

/// Longest message logd keeps; longer ones are cut and counted as malformed.
pub const MAX_MESSAGE: usize = 4096;

#[derive(Debug, PartialEq)]
pub struct Syslog {
    pub facility: u32,
    pub priority: u32,
    pub tag: String,
    pub pid: Option<i32>,
    pub message: String,
}

/// Parses a BSD-style syslog datagram, as glibc's and BusyBox's syslog()
/// send it: `<PRI>Mmm dd hh:mm:ss TAG[PID]: MESSAGE`. The timestamp and the
/// tag are optional; anything without `<PRI>` is user.notice.
///
/// Init sends service output in the same shape, without the timestamp:
/// `<PRI>netd[212]: MESSAGE`.
pub fn syslog(raw: &[u8]) -> Syslog {
    let text = String::from_utf8_lossy(raw);
    let mut s: &str = text.trim_end_matches(['\n', '\r', '\0']);

    let mut pri = 13; // user.notice, as syslog(3) defaults
    if let Some(rest) = s.strip_prefix('<') {
        if let Some(end) = rest.find('>') {
            if let Ok(n) = rest[..end].parse::<u32>() {
                if end <= 3 && n < 192 {
                    pri = n;
                    s = &rest[end + 1..];
                }
            }
        }
    }
    if has_bsd_timestamp(s) {
        s = &s[16..];
    }

    let (tag, pid, message) = split_tag(s);
    Syslog { facility: pri >> 3, priority: pri & 7, tag, pid, message: clean(message) }
}

/// "Nov  1 12:00:00 " or "Nov 01 12:00:00 "
fn has_bsd_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 16
        && b[..3].iter().all(u8::is_ascii_alphabetic)
        && b[3] == b' '
        && (b[4] == b' ' || b[4].is_ascii_digit())
        && b[5].is_ascii_digit()
        && b[6] == b' '
        && b[7].is_ascii_digit()
        && b[8].is_ascii_digit()
        && b[9] == b':'
        && b[12] == b':'
        && b[15] == b' '
}

/// "netd[212]: hello" -> ("netd", Some(212), "hello"); "kernel: x" ->
/// ("kernel", None, "x"); anything else has no tag.
fn split_tag(s: &str) -> (String, Option<i32>, &str) {
    let end = s.find(|c: char| c == ':' || c == '[' || c.is_whitespace()).unwrap_or(s.len());
    let tag = &s[..end];
    if tag.is_empty() || tag.len() > 64 {
        return (String::new(), None, s);
    }
    let rest = &s[end..];
    if let Some(r) = rest.strip_prefix(':') {
        return (tag.to_string(), None, r.strip_prefix(' ').unwrap_or(r));
    }
    if let Some(r) = rest.strip_prefix('[') {
        if let Some(close) = r.find(']') {
            if let Ok(pid) = r[..close].parse::<i32>() {
                let after = &r[close + 1..];
                if let Some(m) = after.strip_prefix(':') {
                    return (tag.to_string(), Some(pid), m.strip_prefix(' ').unwrap_or(m));
                }
            }
        }
    }
    (String::new(), None, s)
}

/// One line, no control characters, at most MAX_MESSAGE bytes.
pub fn clean(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else if c.is_control() { '?' } else { c })
        .collect();
    if out.len() > MAX_MESSAGE {
        let mut cut = MAX_MESSAGE;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

#[derive(Debug, PartialEq)]
pub struct Kmsg {
    pub facility: u32,
    pub priority: u32,
    pub seq: u64,
    /// Microseconds since boot (CLOCK_MONOTONIC).
    pub ts_us: u64,
    pub message: String,
}

/// One /dev/kmsg record: `PRI,SEQ,TIMESTAMP,FLAGS[,...];MESSAGE\n` followed by
/// optional ` KEY=VALUE` continuation lines, which are dropped. Non-printable
/// bytes in MESSAGE arrive as `\xNN`.
pub fn kmsg(raw: &[u8]) -> Option<Kmsg> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once(';')?;
    let mut fields = head.split(',');
    let pri: u32 = fields.next()?.parse().ok()?;
    let seq: u64 = fields.next()?.parse().ok()?;
    let ts_us: u64 = fields.next()?.parse().ok()?;
    let line = body.split('\n').next().unwrap_or("");
    Some(Kmsg { facility: pri >> 3, priority: pri & 7, seq, ts_us, message: clean(&unescape(line)) })
}

fn unescape(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() && b[i + 1] == b'x' {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 2]), hex(b[i + 3])) {
                out.push((h * 16 + l) as u8);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glibc_style() {
        let m = syslog(b"<30>Nov  1 12:00:00 udhcpc[123]: lease of 10.0.2.15 obtained\n");
        assert_eq!(m, Syslog {
            facility: 3,
            priority: 6,
            tag: "udhcpc".into(),
            pid: Some(123),
            message: "lease of 10.0.2.15 obtained".into()
        });
    }

    #[test]
    fn from_init() {
        let m = syslog(b"<27>netd[212]: no carrier on eth0");
        assert_eq!((m.facility, m.priority, m.tag.as_str(), m.pid), (3, 3, "netd", Some(212)));
        assert_eq!(m.message, "no carrier on eth0");
    }

    #[test]
    fn busybox_logger_without_pid() {
        let m = syslog(b"<13>Nov 01 09:05:07 root: hello there");
        assert_eq!((m.tag.as_str(), m.pid, m.message.as_str()), ("root", None, "hello there"));
    }

    #[test]
    fn bare_and_odd() {
        let m = syslog(b"just some text");
        assert_eq!((m.facility, m.priority, m.tag.as_str()), (1, 5, ""));
        assert_eq!(m.message, "just some text");
        // not a priority: kept as text
        let m = syslog(b"<abc> hi");
        assert_eq!(m.message, "<abc> hi");
        let m = syslog(b"<999>x");
        assert_eq!(m.priority, 5);
        // a colon later in the text isn't a tag
        let m = syslog(b"<14>time is 12:00");
        assert_eq!((m.tag.as_str(), m.message.as_str()), ("", "time is 12:00"));
        // control characters
        let m = syslog(b"<14>a: one\ntwo\x07");
        assert_eq!(m.message, "one two?");
    }

    #[test]
    fn long_messages_are_cut_on_a_char_boundary() {
        let s = "é".repeat(MAX_MESSAGE);
        let c = clean(&s);
        assert!(c.len() <= MAX_MESSAGE && c.len() >= MAX_MESSAGE - 1);
    }

    #[test]
    fn kernel_records() {
        let k = kmsg(b"6,339,5140900,-;virtio_net virtio0 eth0: renamed\n SUBSYSTEM=net\n DEVICE=n2\n").unwrap();
        assert_eq!(k, Kmsg { facility: 0, priority: 6, seq: 339, ts_us: 5140900, message: "virtio_net virtio0 eth0: renamed".into() });
        let k = kmsg(b"4,1,2,-,caller=T1;tab\\x09here").unwrap();
        assert_eq!((k.priority, k.message.as_str()), (4, "tab here"));
        assert!(kmsg(b"garbage").is_none());
    }
}
