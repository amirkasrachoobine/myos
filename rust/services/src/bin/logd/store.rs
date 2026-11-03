//! Where entries go: a ring of recent entries in memory, for queries over
//! the bus, and a text file on disk, rotated by size.

use myos_services::format_time_us;
use myos_services::proto::log::{Entry, QueryReply, QueryRequest, StatsReply};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 1000;

pub struct Store {
    ring: VecDeque<Entry>,
    capacity: usize,
    next_seq: u64,
    received: u64,
    pub malformed: u64,
    file: Option<LogFile>,
}

impl Store {
    pub fn new(capacity: usize, file: Option<LogFile>) -> Store {
        Store { ring: VecDeque::with_capacity(capacity.min(4096)), capacity: capacity.max(1), next_seq: 1, received: 0, malformed: 0, file }
    }

    /// Stores `e`, assigning its seq. Returns the seq.
    pub fn add(&mut self, mut e: Entry) -> u64 {
        e.seq = self.next_seq;
        self.next_seq += 1;
        self.received += 1;
        if let Some(f) = &mut self.file {
            f.write(&e);
        }
        if self.ring.len() == self.capacity {
            self.ring.pop_front();
        }
        self.ring.push_back(e);
        self.next_seq - 1
    }

    pub fn query(&self, q: &QueryRequest, caller_uid: u32) -> QueryReply {
        let limit = match q.limit as usize {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        };
        let wanted = |e: &&Entry| {
            (caller_uid == 0 || e.uid == caller_uid)
                && q.max_priority.map_or(true, |p| e.priority <= p)
                && (q.tag.is_empty() || e.tag == q.tag)
                && (q.contains.is_empty() || e.message.contains(&q.contains))
        };
        let entries: Vec<Entry> = if q.after_seq == 0 {
            let mut v: Vec<Entry> = self.ring.iter().rev().filter(wanted).take(limit).cloned().collect();
            v.reverse();
            v
        } else {
            // Entries are in seq order, so skip straight to after_seq.
            let start = self.ring.partition_point(|e| e.seq <= q.after_seq);
            self.ring.range(start..).filter(wanted).take(limit).cloned().collect()
        };
        QueryReply { entries, last_seq: self.next_seq - 1, first_seq: self.ring.front().map_or(self.next_seq, |e| e.seq) }
    }

    pub fn stats(&self) -> StatsReply {
        StatsReply {
            received: self.received,
            in_memory: self.ring.len() as u64,
            memory_capacity: self.capacity as u64,
            first_seq: self.ring.front().map_or(self.next_seq, |e| e.seq),
            last_seq: self.next_seq - 1,
            file: self.file.as_ref().map(|f| f.path.display().to_string()).unwrap_or_default(),
            file_size: self.file.as_ref().map_or(0, |f| f.size),
            malformed: self.malformed,
        }
    }
}

/// One line per entry:
/// `2026-11-01 04:13:04.404 info    netd[212]: got lease 10.0.2.15/24`
pub fn format_line(e: &Entry) -> String {
    let who = match (e.tag.is_empty(), e.pid) {
        (true, _) => "-".to_string(),
        (false, 0) => e.tag.clone(),
        (false, pid) => format!("{}[{pid}]", e.tag),
    };
    format!("{} {:<7} {who}: {}", format_time_us(e.time_us), myos_services::priority_name(e.priority), e.message)
}

/// The log file. When it grows past `max_bytes` it's renamed to `<path>.1`
/// (replacing the previous one) and a new file is started.
pub struct LogFile {
    pub path: PathBuf,
    file: Option<File>,
    pub size: u64,
    max_bytes: u64,
    failed: bool,
}

impl LogFile {
    pub fn open(path: PathBuf, max_bytes: u64) -> LogFile {
        let mut f = LogFile { path, file: None, size: 0, max_bytes: max_bytes.max(4096), failed: false };
        f.reopen();
        f
    }

    fn reopen(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(file) => {
                self.size = file.metadata().map(|m| m.len()).unwrap_or(0);
                self.file = Some(file);
                self.failed = false;
            }
            Err(e) => {
                if !self.failed {
                    myos_services::error!("can't open {}: {e}; keeping logs in memory only", self.path.display());
                    self.failed = true;
                }
                self.file = None;
            }
        }
    }

    fn write(&mut self, e: &Entry) {
        if self.file.is_none() {
            // e.g. / was still read-only, or /var/log was missing; try again.
            self.reopen();
        }
        let Some(file) = &mut self.file else { return };
        let mut line = format_line(e);
        line.push('\n');
        if file.write_all(line.as_bytes()).is_err() {
            self.file = None;
            return;
        }
        self.size += line.len() as u64;
        if self.size >= self.max_bytes {
            let mut old = self.path.clone().into_os_string();
            old.push(".1");
            let _ = fs::rename(&self.path, &old);
            self.file = None;
            self.reopen();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: &str, pri: u32, uid: u32, msg: &str) -> Entry {
        Entry { tag: tag.into(), priority: pri, uid, message: msg.into(), time_us: 0, pid: 7, ..Default::default() }
    }

    #[test]
    fn ring_queries() {
        let mut s = Store::new(5, None);
        for i in 1..=8 {
            s.add(entry(if i % 2 == 0 { "even" } else { "odd" }, if i == 3 { 3 } else { 6 }, if i == 8 { 1000 } else { 0 }, &format!("m{i}")));
        }
        let msgs = |r: QueryReply| r.entries.iter().map(|e| e.message.clone()).collect::<Vec<_>>();
        // only 4..=8 are kept
        assert_eq!(msgs(s.query(&QueryRequest::default(), 0)), ["m4", "m5", "m6", "m7", "m8"]);
        let r = s.query(&QueryRequest::default(), 0);
        assert_eq!((r.first_seq, r.last_seq), (4, 8));
        // tail with limit
        assert_eq!(msgs(s.query(&QueryRequest { limit: 2, ..Default::default() }, 0)), ["m7", "m8"]);
        // forward from a seq
        assert_eq!(msgs(s.query(&QueryRequest { after_seq: 5, limit: 2, ..Default::default() }, 0)), ["m6", "m7"]);
        assert_eq!(msgs(s.query(&QueryRequest { after_seq: 8, ..Default::default() }, 0)), Vec::<String>::new());
        // filters
        assert_eq!(msgs(s.query(&QueryRequest { tag: "odd".into(), ..Default::default() }, 0)), ["m5", "m7"]);
        assert_eq!(msgs(s.query(&QueryRequest { contains: "8".into(), ..Default::default() }, 0)), ["m8"]);
        // non-root callers only see their own entries
        assert_eq!(msgs(s.query(&QueryRequest::default(), 1000)), ["m8"]);
        // severity
        let mut s2 = Store::new(10, None);
        s2.add(entry("a", 3, 0, "bad"));
        s2.add(entry("a", 6, 0, "fine"));
        assert_eq!(msgs(s2.query(&QueryRequest { max_priority: Some(4), ..Default::default() }, 0)), ["bad"]);
    }

    #[test]
    fn file_rotates() {
        let dir = std::env::temp_dir().join(format!("logd-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("messages");
        let mut s = Store::new(10, Some(LogFile::open(path.clone(), 4096)));
        for i in 0..100 {
            s.add(entry("t", 6, 0, &format!("line {i} {}", "x".repeat(50))));
        }
        let cur = fs::read_to_string(&path).unwrap();
        let old = fs::read_to_string(dir.join("messages.1")).unwrap();
        assert!(old.len() >= 4096 && cur.len() < 4096, "{} {}", old.len(), cur.len());
        assert!(cur.lines().last().unwrap().ends_with(&format!("t[7]: line 99 {}", "x".repeat(50))));
        assert!(old.starts_with("1970-01-01 00:00:00.000 info    t[7]: line "));
        fs::remove_dir_all(&dir).unwrap();
    }
}
