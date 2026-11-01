//! Passing service output on to logd (Phase 3).
//!
//! Every service with `output = "log"` (the default) gets a pipe as its
//! stdout and stderr. Init reads the pipes in its main loop and sends each
//! line to logd as a syslog datagram on /dev/log:
//!
//!     <30>netd[212]: eth0: 10.0.2.15/24 from 10.0.2.2
//!
//! logd sees from the kernel that the datagram came from PID 1 and trusts
//! the tag and pid in it. A line may start with a syslog priority, as with
//! systemd: `<3>something failed` is logged at err; the default is info.
//!
//! Init's own messages go the same way, tagged `init`, as well as to the
//! console.
//!
//! Until logd is listening (at boot, or while it restarts) lines wait in a
//! backlog of up to 1000 and are also shown on the console, so nothing is
//! lost if logd never starts. Init never blocks on logd: if its socket is
//! full, lines wait in the backlog too.
//!
//! MYOS_INIT_LOG_SOCKET overrides /dev/log (tests); "none" turns this off
//! and sends service output straight to the console, as in Phase 1.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const BACKLOG: usize = 1000;
/// Longer lines are split.
pub const MAX_LINE: usize = 2048;
const FACILITY_DAEMON: u32 = 3;
const INFO: u32 = 6;

/// Set once we're PID 1, so `initctl` (the same binary) never forwards.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FWD: Mutex<Option<Forwarder>> = Mutex::new(None);

pub struct Forwarder {
    path: PathBuf,
    sock: Option<UnixDatagram>,
    backlog: VecDeque<Vec<u8>>,
    dropped: u64,
}

/// Where service output goes, from MYOS_INIT_LOG_SOCKET. None: off.
pub fn socket_path() -> Option<PathBuf> {
    match std::env::var("MYOS_INIT_LOG_SOCKET") {
        Ok(v) if v == "none" || v.is_empty() => None,
        Ok(v) => Some(v.into()),
        Err(_) => Some("/dev/log".into()),
    }
}

/// Called once, early in boot, as PID 1.
pub fn activate() {
    if let Some(path) = socket_path() {
        *FWD.lock().unwrap() = Some(Forwarder { path, sock: None, backlog: VecDeque::new(), dropped: 0 });
        ACTIVE.store(true, Ordering::Relaxed);
    }
}

pub fn enabled() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// One line of output from service `tag` (main pid `pid`).
pub fn line(tag: &str, pid: i32, raw: &[u8]) {
    if !enabled() {
        return;
    }
    let (level, text) = split_priority(raw);
    let dgram = format!("<{}>{tag}[{pid}]: {}", FACILITY_DAEMON * 8 + level, String::from_utf8_lossy(text)).into_bytes();
    if let Some(f) = FWD.lock().unwrap().as_mut() {
        f.send(dgram, Some((tag, text)));
    }
}

/// One of init's own messages (already on the console).
pub fn own(msg: &str) {
    if !enabled() {
        return;
    }
    let dgram = format!("<{}>init[1]: {msg}", FACILITY_DAEMON * 8 + INFO).into_bytes();
    if let Some(f) = FWD.lock().unwrap().as_mut() {
        f.send(dgram, None);
    }
}

/// Retries the backlog. Returns true while lines are still waiting.
pub fn flush() -> bool {
    if !enabled() {
        return false;
    }
    match FWD.lock().unwrap().as_mut() {
        Some(f) => {
            f.flush();
            !f.backlog.is_empty()
        }
        None => false,
    }
}

pub fn pending() -> bool {
    enabled() && FWD.lock().unwrap().as_ref().is_some_and(|f| !f.backlog.is_empty())
}

/// `<3>text` -> (3, "text"); anything else is info.
pub fn split_priority(raw: &[u8]) -> (u32, &[u8]) {
    if raw.len() >= 3 && raw[0] == b'<' && raw[2] == b'>' && (b'0'..=b'7').contains(&raw[1]) {
        ((raw[1] - b'0') as u32, &raw[3..])
    } else {
        (INFO, raw)
    }
}

impl Forwarder {
    fn try_send(&mut self, d: &[u8]) -> io::Result<()> {
        if self.sock.is_none() {
            let s = UnixDatagram::unbound()?;
            s.set_nonblocking(true)?;
            self.sock = Some(s);
        }
        self.sock.as_ref().unwrap().send_to(d, &self.path).map(drop)
    }

    /// Sends what's waiting, oldest first. Stops at the first failure.
    fn flush(&mut self) -> bool {
        if self.dropped > 0 {
            let note = format!(
                "<{}>init[1]: {} lines of service output were dropped while logd was unavailable",
                FACILITY_DAEMON * 8 + 4,
                self.dropped
            );
            if self.try_send(note.as_bytes()).is_err() {
                return false;
            }
            self.dropped = 0;
        }
        while let Some(d) = self.backlog.front() {
            let d = d.clone();
            if self.try_send(&d).is_err() {
                return false;
            }
            self.backlog.pop_front();
        }
        true
    }

    /// `echo`: the service and text to show on the console if logd isn't
    /// listening (init's own messages are on the console already).
    fn send(&mut self, d: Vec<u8>, echo: Option<(&str, &[u8])>) {
        if self.flush() {
            match self.try_send(&d) {
                Ok(()) => return,
                // Full: logd is busy. It'll catch up; don't show this on the console.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.queue(d);
                    return;
                }
                Err(_) => {}
            }
        }
        if let Some((tag, text)) = echo {
            let mut err = io::stderr().lock();
            let _ = err.write_all(format!("{tag}: ").as_bytes());
            let _ = err.write_all(text);
            let _ = err.write_all(b"\n");
        }
        self.queue(d);
    }

    fn queue(&mut self, d: Vec<u8>) {
        if self.backlog.len() >= BACKLOG {
            self.backlog.pop_front();
            self.dropped += 1;
        }
        self.backlog.push_back(d);
    }
}

/// Splits what a service wrote into lines, keeping an incomplete last line
/// for next time. Lines longer than MAX_LINE are split; blank lines are
/// dropped, as they'd only be noise in the log.
pub struct LineBuffer {
    partial: Vec<u8>,
}

impl LineBuffer {
    pub fn new() -> LineBuffer {
        LineBuffer { partial: Vec::new() }
    }

    pub fn push(&mut self, data: &[u8], mut each: impl FnMut(&[u8])) {
        self.partial.extend_from_slice(data);
        let mut start = 0;
        while let Some(nl) = self.partial[start..].iter().position(|&b| b == b'\n') {
            let end = start + nl;
            for chunk in self.partial[start..end].chunks(MAX_LINE) {
                each(chunk.strip_suffix(b"\r").unwrap_or(chunk));
            }
            start = end + 1;
        }
        self.partial.drain(..start);
        while self.partial.len() >= MAX_LINE {
            let rest = self.partial.split_off(MAX_LINE);
            each(&self.partial);
            self.partial = rest;
        }
    }

    /// At end of file: whatever is left, if anything.
    pub fn finish(&mut self, mut each: impl FnMut(&[u8])) {
        if !self.partial.is_empty() {
            each(&self.partial);
            self.partial.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(chunks: &[&[u8]]) -> Vec<String> {
        let mut out = Vec::new();
        let mut b = LineBuffer::new();
        for c in chunks {
            b.push(c, |l| out.push(String::from_utf8_lossy(l).into_owned()));
        }
        b.finish(|l| out.push(String::from_utf8_lossy(l).into_owned()));
        out
    }

    #[test]
    fn splits_lines() {
        assert_eq!(lines(&[b"one\ntw", b"o\r\nthree"]), ["one", "two", "three"]);
        assert_eq!(lines(&[b"a\n\n\nb\n"]), ["a", "b"]);
        let long = vec![b'x'; MAX_LINE * 2 + 5];
        let got = lines(&[&long]);
        assert_eq!(got.iter().map(String::len).collect::<Vec<_>>(), [MAX_LINE, MAX_LINE, 5]);
    }

    #[test]
    fn priorities() {
        assert_eq!(split_priority(b"<3>bad"), (3, &b"bad"[..]));
        assert_eq!(split_priority(b"<9>x"), (6, &b"<9>x"[..]));
        assert_eq!(split_priority(b"plain"), (6, &b"plain"[..]));
    }

    #[test]
    fn backlog_until_listening_then_in_order() {
        let dir = std::env::temp_dir().join(format!("logfwd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("log.sock");
        let _ = std::fs::remove_file(&path);
        let mut f = Forwarder { path: path.clone(), sock: None, backlog: VecDeque::new(), dropped: 0 };
        for i in 0..(BACKLOG + 3) {
            f.send(format!("m{i}").into_bytes(), None);
        }
        assert_eq!((f.backlog.len(), f.dropped), (BACKLOG, 3));
        let rx = UnixDatagram::bind(&path).unwrap();
        // Sends until the receiver's queue is full; the rest keeps waiting.
        f.flush();
        let mut buf = [0u8; 256];
        let n = rx.recv(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("3 lines of service output were dropped"));
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"m3");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
