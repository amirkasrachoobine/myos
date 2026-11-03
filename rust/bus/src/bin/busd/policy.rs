//! What Phase 6 apps may do on the bus.
//!
//! Every app runs under its own uid in APP_UID_MIN..=APP_UID_MAX (jrun
//! allocates them). For a connection with such a uid, busd reads
//! `<policy dir>/<uid>` (default /etc/myos/bus/policy.d/<uid>), which jrun
//! writes when it installs the app:
//!
//! ```text
//! # comments
//! app org.example.notes      the app's id: it may register app.org.example.notes
//!                            and names under it (app.org.example.notes.*)
//! call log                   services it may call, exact names...
//! call app.org.example.*     ...or a prefix ending in ".*"
//! ```
//!
//! An app may always call "bus" (Ping, ListServices) and its own names. With
//! no policy file, or one that isn't owned by root or is writable by anyone
//! else, the app may do nothing else: the check fails closed.
//!
//! Other uids are unaffected: root may do anything, other users may call
//! anything and register names under "app." (services do their own checks
//! with the caller's uid).

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

pub use myos_bus::{APP_UID_MAX, APP_UID_MIN};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppPolicy {
    pub app: String,
    pub calls: Vec<String>,
}

impl AppPolicy {
    pub fn parse(text: &str) -> Result<AppPolicy, String> {
        let mut p = AppPolicy::default();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once(char::is_whitespace).map(|(k, v)| (k, v.trim())).unwrap_or((line, ""));
            match key {
                "app" if !value.is_empty() && !value.contains(char::is_whitespace) => p.app = value.to_string(),
                "call" if !value.is_empty() && !value.contains(char::is_whitespace) => p.calls.push(value.to_string()),
                _ => return Err(format!("line {}: expected `app ID` or `call SERVICE`", n + 1)),
            }
        }
        if p.app.is_empty() {
            return Err("no `app` line".into());
        }
        Ok(p)
    }

    fn own(&self, name: &str) -> bool {
        let base = format!("app.{}", self.app);
        name == base || name.strip_prefix(&base).is_some_and(|rest| rest.starts_with('.'))
    }

    pub fn may_call(&self, service: &str) -> bool {
        service == myos_bus::BUS_SERVICE
            || self.own(service)
            || self.calls.iter().any(|c| match c.strip_suffix(".*") {
                Some(prefix) => service.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.')),
                None => c == service,
            })
    }

    pub fn may_register(&self, name: &str) -> bool {
        self.own(name)
    }
}

pub fn is_app_uid(uid: u32) -> bool {
    (APP_UID_MIN..=APP_UID_MAX).contains(&uid)
}

pub struct Policies {
    dir: PathBuf,
    // uid -> (mtime, size, inode) of the file it was read from, and the result
    cache: Mutex<HashMap<u32, ((SystemTime, u64, u64), Result<AppPolicy, String>)>>,
}

impl Policies {
    pub fn new(dir: PathBuf) -> Policies {
        Policies { dir, cache: Mutex::new(HashMap::new()) }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// The policy for an app uid, re-read whenever its file changes.
    /// Err says why the app gets nothing.
    pub fn get(&self, uid: u32) -> Result<AppPolicy, String> {
        let path = self.dir.join(uid.to_string());
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                self.cache.lock().unwrap_or_else(|p| p.into_inner()).remove(&uid);
                return Err(format!("app uid {uid} has no bus policy ({}: {e})", path.display()));
            }
        };
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 || !meta.is_file() {
            return Err(format!("ignoring {}: it must be a regular file owned by root and writable only by root", path.display()));
        }
        let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len(), meta.ino());
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((s, p)) = cache.get(&uid) {
            if *s == stamp {
                return p.clone();
            }
        }
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| AppPolicy::parse(&t))
            .map_err(|e| format!("{}: {e}", path.display()));
        cache.insert(uid, (stamp, parsed.clone()));
        parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_check() {
        let p = AppPolicy::parse("# hi\napp org.ex.notes\ncall log\ncall app.org.ex.*  # others\n\ncall sysinfo\n").unwrap();
        assert_eq!(p.app, "org.ex.notes");
        assert!(p.may_call("bus"));
        assert!(p.may_call("log"));
        assert!(p.may_call("sysinfo"));
        assert!(!p.may_call("net"));
        assert!(!p.may_call("logx"));
        assert!(p.may_call("app.org.ex.notes"));
        assert!(p.may_call("app.org.ex.other"));
        assert!(!p.may_call("app.org.ex"));
        assert!(!p.may_call("app.org.exfoo.x"));
        assert!(p.may_register("app.org.ex.notes"));
        assert!(p.may_register("app.org.ex.notes.sync"));
        assert!(!p.may_register("app.org.ex.notesx"));
        assert!(!p.may_register("app.org.ex.other"));
        assert!(!p.may_register("log"));
    }

    #[test]
    fn bad_files() {
        assert!(AppPolicy::parse("call log\n").is_err());
        assert!(AppPolicy::parse("app a\nallow log\n").is_err());
        assert!(AppPolicy::parse("app a b\n").is_err());
        assert!(AppPolicy::parse("app a\ncall\n").is_err());
    }
}
