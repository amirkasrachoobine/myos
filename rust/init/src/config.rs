//! Service definitions: one TOML file per service in /etc/myos/services/.
//!
//! ```toml
//! # /etc/myos/services/syslogd.toml   (the service name is the file name)
//! description = "System log daemon"
//! exec = ["/sbin/syslogd", "-n"]   # absolute path, no shell, no PATH lookup
//! type = "simple"                  # "simple" (long-running) or "oneshot" (runs once at boot)
//! restart = "always"               # "always", "on-failure" (default) or "never"; simple only
//! after = ["network"]              # start only once these are up (oneshot: finished)
//! enabled = true                   # false keeps the file but doesn't start it
//! console = false                  # true: stdin/stdout/stderr all on /dev/console
//! cwd = "/"
//! stop_signal = "TERM"             # sent to the service's process group on stop
//! stop_timeout = 5                 # seconds before SIGKILL
//!
//! [env]
//! LANG = "C.UTF-8"
//! ```

use crate::sys;
use crate::toml::{self, Table, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::os::raw::c_int;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Simple,
    Oneshot,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Restart {
    Always,
    OnFailure,
    Never,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Service {
    pub name: String,
    pub description: String,
    pub exec: Vec<String>,
    pub kind: Kind,
    pub restart: Restart,
    pub after: Vec<String>,
    pub enabled: bool,
    pub console: bool,
    pub cwd: String,
    pub stop_signal: c_int,
    pub stop_timeout: Duration,
    pub env: Vec<(String, String)>,
}

const KEYS: &[&str] = &[
    "description", "exec", "type", "restart", "after", "enabled", "console", "cwd", "stop_signal", "stop_timeout",
];

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.' | '@'))
        && !name.starts_with('.')
}

pub fn parse_service(name: &str, text: &str) -> Result<Service, String> {
    if !valid_name(name) {
        return Err(format!("invalid service name `{name}` (use lowercase letters, digits, - _ . @)"));
    }
    let doc = toml::parse(text)?;
    for t in doc.tables.keys() {
        if t != "env" {
            return Err(format!("unknown table `[{t}]` (only [env] is allowed)"));
        }
    }
    let r = &doc.root;
    for k in r.keys() {
        if !KEYS.contains(&k.as_str()) {
            return Err(format!("unknown key `{k}` (known keys: {})", KEYS.join(", ")));
        }
    }

    let exec = string_list(r, "exec")?.ok_or("missing required key `exec`")?;
    if exec.is_empty() {
        return Err("`exec` must not be empty".into());
    }
    if !exec[0].starts_with('/') {
        return Err(format!("`exec` must start with an absolute path, got `{}`", exec[0]));
    }

    let kind = match string(r, "type")?.as_deref() {
        None | Some("simple") => Kind::Simple,
        Some("oneshot") => Kind::Oneshot,
        Some(other) => return Err(format!("`type` must be \"simple\" or \"oneshot\", got \"{other}\"")),
    };
    let restart = match string(r, "restart")?.as_deref() {
        None => if kind == Kind::Oneshot { Restart::Never } else { Restart::OnFailure },
        Some("always") => Restart::Always,
        Some("on-failure") => Restart::OnFailure,
        Some("never") => Restart::Never,
        Some(other) => return Err(format!("`restart` must be \"always\", \"on-failure\" or \"never\", got \"{other}\"")),
    };
    if kind == Kind::Oneshot && restart != Restart::Never {
        return Err("oneshot services can't restart; remove `restart`".into());
    }

    let after = string_list(r, "after")?.unwrap_or_default();
    for dep in &after {
        if dep == name {
            return Err("a service can't be `after` itself".into());
        }
    }

    let stop_signal = match string(r, "stop_signal")? {
        None => sys::SIGTERM,
        Some(s) => sys::signal_from_name(&s.to_ascii_uppercase())
            .ok_or_else(|| format!("unknown `stop_signal` \"{s}\" (use TERM, HUP, INT, QUIT, USR1, USR2 or KILL)"))?,
    };
    let stop_timeout = match r.get("stop_timeout") {
        None => 5,
        Some(Value::Int(n)) if (1..=300).contains(n) => *n as u64,
        Some(Value::Int(n)) => return Err(format!("`stop_timeout` must be 1–300 seconds, got {n}")),
        Some(v) => return Err(format!("`stop_timeout` must be an integer, got {}", v.type_name())),
    };

    let mut env = Vec::new();
    if let Some(t) = doc.tables.get("env") {
        for (k, v) in t {
            let ok = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(format!("invalid environment variable name `{k}`"));
            }
            match v {
                Value::Str(s) => env.push((k.clone(), s.clone())),
                other => return Err(format!("[env] {k} must be a string, got {}", other.type_name())),
            }
        }
    }

    Ok(Service {
        name: name.to_string(),
        description: string(r, "description")?.unwrap_or_default(),
        exec,
        kind,
        restart,
        after,
        enabled: boolean(r, "enabled")?.unwrap_or(true),
        console: boolean(r, "console")?.unwrap_or(false),
        cwd: string(r, "cwd")?.unwrap_or_else(|| "/".into()),
        stop_signal,
        stop_timeout: Duration::from_secs(stop_timeout),
        env,
    })
}

fn string(t: &Table, key: &str) -> Result<Option<String>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s.clone())),
        Some(v) => Err(format!("`{key}` must be a string, got {}", v.type_name())),
    }
}

fn boolean(t: &Table, key: &str) -> Result<Option<bool>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(v) => Err(format!("`{key}` must be true or false, got {}", v.type_name())),
    }
}

fn string_list(t: &Table, key: &str) -> Result<Option<Vec<String>>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::Str(s) => Ok(s.clone()),
                other => Err(format!("`{key}` must be a list of strings, found a {}", other.type_name())),
            })
            .collect::<Result<_, _>>()
            .map(Some),
        Some(v) => Err(format!("`{key}` must be a list of strings, got {}", v.type_name())),
    }
}

/// Result of loading a service directory: the good services, plus one
/// message per file that was skipped or per dependency problem found.
pub struct Loaded {
    pub services: Vec<Service>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn load_dir(dir: &Path) -> Loaded {
    let mut services = Vec::new();
    let mut errors = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(format!("{}: {e}", dir.display()));
            return Loaded { services, errors, warnings: Vec::new() };
        }
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".toml")) else {
            continue;
        };
        let result = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| parse_service(name, &t));
        match result {
            Ok(s) => services.push(s),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    let warnings = check_dependencies(&mut services);
    Loaded { services, errors, warnings }
}

/// Warn about `after` entries naming unknown services (they're ignored at
/// runtime) and break dependency cycles, which would otherwise keep every
/// service in the cycle waiting forever.
pub fn check_dependencies(services: &mut [Service]) -> Vec<String> {
    let mut warnings = Vec::new();
    let names: BTreeSet<String> = services.iter().map(|s| s.name.clone()).collect();
    for s in services.iter_mut() {
        let (known, unknown): (Vec<_>, Vec<_>) = s.after.drain(..).partition(|d| names.contains(d));
        for d in unknown {
            warnings.push(format!("{}: `after` names unknown service `{d}`; ignoring it", s.name));
        }
        s.after = known;
    }

    // Kahn's algorithm: whatever can't be ordered is in (or behind) a cycle.
    let mut pending: BTreeMap<&str, usize> = services.iter().map(|s| (s.name.as_str(), s.after.len())).collect();
    let mut done: Vec<String> = Vec::new();
    loop {
        let ready: Vec<String> = pending.iter().filter(|(_, n)| **n == 0).map(|(k, _)| k.to_string()).collect();
        if ready.is_empty() {
            break;
        }
        for r in &ready {
            pending.remove(r.as_str());
            for s in services.iter() {
                if s.after.iter().any(|d| d == r) {
                    if let Some(n) = pending.get_mut(s.name.as_str()) {
                        *n -= 1;
                    }
                }
            }
        }
        done.extend(ready);
    }
    let mut stuck: BTreeSet<String> = pending.keys().map(|k| k.to_string()).collect();
    // Peel off services that are only *behind* a cycle (nothing stuck waits
    // on them); they keep their `after` and start once the cycle is broken.
    loop {
        let leaf = stuck
            .iter()
            .find(|n| !services.iter().any(|s| stuck.contains(&s.name) && s.after.contains(n)))
            .cloned();
        match leaf {
            Some(n) => {
                stuck.remove(&n);
            }
            None => break,
        }
    }
    if !stuck.is_empty() {
        let list = stuck.iter().cloned().collect::<Vec<_>>().join(", ");
        warnings.push(format!("dependency cycle among: {list}; ignoring their `after` entries between each other"));
        for s in services.iter_mut().filter(|s| stuck.contains(&s.name)) {
            s.after.retain(|d| !stuck.contains(d));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, after: &[&str]) -> Service {
        let after = after.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(", ");
        parse_service(name, &format!("exec = [\"/bin/true\"]\nafter = [{after}]")).unwrap()
    }

    #[test]
    fn defaults() {
        let s = parse_service("x", "exec = [\"/bin/sleep\", \"10\"]").unwrap();
        assert_eq!(s.kind, Kind::Simple);
        assert_eq!(s.restart, Restart::OnFailure);
        assert!(s.enabled && !s.console);
        assert_eq!(s.cwd, "/");
        assert_eq!(s.stop_signal, sys::SIGTERM);
        assert_eq!(s.stop_timeout, Duration::from_secs(5));
        let o = parse_service("y", "exec = [\"/bin/true\"]\ntype = \"oneshot\"").unwrap();
        assert_eq!(o.restart, Restart::Never);
    }

    #[test]
    fn full() {
        let s = parse_service(
            "getty",
            "exec = [\"/sbin/getty\", \"-L\", \"115200\", \"ttyS0\"]\nrestart = \"always\"\nstop_signal = \"hup\"\nstop_timeout = 2\nafter = [\"a\"]\n[env]\nTERM = \"vt100\"\n",
        )
        .unwrap();
        assert_eq!(s.restart, Restart::Always);
        assert_eq!(s.stop_signal, sys::SIGHUP);
        assert_eq!(s.env, vec![("TERM".to_string(), "vt100".to_string())]);
    }

    #[test]
    fn rejects_mistakes() {
        let cases = [
            ("Bad", "exec = [\"/bin/true\"]", "invalid service name"),
            ("x", "", "missing required key `exec`"),
            ("x", "exec = []", "must not be empty"),
            ("x", "exec = [\"true\"]", "absolute path"),
            ("x", "exec = \"/bin/true\"", "list of strings"),
            ("x", "exec = [\"/bin/true\"]\nrestrat = \"always\"", "unknown key `restrat`"),
            ("x", "exec = [\"/bin/true\"]\ntype = \"oneshot\"\nrestart = \"always\"", "oneshot services can't restart"),
            ("x", "exec = [\"/bin/true\"]\nrestart = \"sometimes\"", "`restart` must be"),
            ("x", "exec = [\"/bin/true\"]\nstop_timeout = 0", "1–300"),
            ("x", "exec = [\"/bin/true\"]\nstop_signal = \"STOP\"", "unknown `stop_signal`"),
            ("x", "exec = [\"/bin/true\"]\nafter = [\"x\"]", "after` itself"),
            ("x", "exec = [\"/bin/true\"]\n[envv]", "unknown table"),
            ("x", "exec = [\"/bin/true\"]\n[env]\n\"1X\" = \"a\"", "invalid environment variable"),
        ];
        for (name, src, want) in cases {
            let err = parse_service(name, src).unwrap_err();
            assert!(err.contains(want), "{src:?}: got {err:?}, want {want:?}");
        }
    }

    #[test]
    fn unknown_deps_and_cycles() {
        let mut s = vec![svc("a", &["b"]), svc("b", &["c"]), svc("c", &["a"]), svc("d", &["a", "ghost"]), svc("e", &[])];
        let w = check_dependencies(&mut s);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("unknown service `ghost`"));
        assert!(w[1].contains("cycle among: a, b, c;"), "{w:?}");
        // d isn't in the cycle itself, so it keeps waiting for a.
        assert_eq!(s[3].after, vec!["a".to_string()]);
        assert!(s[0].after.is_empty() && s[1].after.is_empty() && s[2].after.is_empty());
    }
}
