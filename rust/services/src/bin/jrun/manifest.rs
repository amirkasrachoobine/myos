//! app.toml, the manifest at the top of every app package.
//!
//! ```toml
//! id = "org.myos.hello"            # required: reverse-DNS, a-z 0-9 . -
//! name = "Hello"                   # shown by jrun list
//! version = "1.0.0"                # required
//! description = "..."
//! main = "org.myos.hello.Hello"    # required: an org.myos.sdk.App, or a class with main(String[])
//! classpath = ["hello.jar"]        # jars in the package; default ["app.jar"]
//! args = []                        # arguments for main
//! permissions = ["log", "sysinfo"] # see PERMISSIONS
//! memory = 64                      # MiB of Java heap (-Xmx), 16-2048
//! service = true                   # run as a service (app.<id>); false: only `jrun run`
//! restart = "on-failure"           # "always" | "on-failure" | "never"
//! ```

#[path = "../../../../init/src/toml.rs"]
#[allow(dead_code)]
mod toml;

use toml::{Table, Value};

/// What an app may ask for. Anything not granted is refused by the system
/// (busd, the kernel), not just by the SDK.
pub const PERMISSIONS: &[(&str, &str)] = &[
    ("log", "write to the system log (log.Write) and read its own entries"),
    ("sysinfo", "read CPU, memory, disk and process information (the sysinfo service)"),
    ("netinfo", "read network interface status (the net service)"),
    ("network", "open network connections; without it the app has only its own loopback"),
    ("call:SERVICE", "call another bus service, e.g. call:app.org.example.notes or call:app.org.example.*"),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub main: String,
    pub classpath: Vec<String>,
    pub args: Vec<String>,
    pub permissions: Vec<String>,
    pub memory_mb: u32,
    pub service: bool,
    pub restart: String,
}

const KEYS: &[&str] = &[
    "id", "name", "version", "description", "main", "classpath", "args", "permissions", "memory", "service", "restart",
];

/// Reverse-DNS app ids: two or more dot-separated parts of a-z 0-9 -,
/// each starting with a letter; at most 60 characters, so that
/// "app.<id>" fits the 64-character limit on service and bus names.
pub fn valid_id(id: &str) -> Result<(), String> {
    let parts: Vec<&str> = id.split('.').collect();
    let part_ok = |p: &&str| {
        p.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if id.len() > 60 || parts.len() < 2 || !parts.iter().all(part_ok) {
        return Err(format!(
            "bad app id {id:?}: use reverse-DNS like \"org.example.notes\" (a-z, 0-9 and - in each part, at most 60 characters)"
        ));
    }
    Ok(())
}

fn valid_class(c: &str) -> bool {
    !c.is_empty()
        && c.split('.').all(|p| {
            p.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && p.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
}

/// Bus services an app with these permissions may call (busd policy lines).
pub fn bus_calls(perms: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in perms {
        let s = match p.as_str() {
            "log" => "log",
            "sysinfo" => "sysinfo",
            "netinfo" => "net",
            p => match p.strip_prefix("call:") {
                Some(s) => s,
                None => continue,
            },
        };
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    }
    out
}

fn check_permission(p: &str) -> Result<(), String> {
    if let Some(svc) = p.strip_prefix("call:") {
        let name = svc.strip_suffix(".*").unwrap_or(svc);
        let ok = !name.is_empty()
            && name.len() <= 64
            && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
        if !ok || name == "bus" {
            return Err(format!("bad permission {p:?}: call: needs a bus service name, like call:app.org.example.notes"));
        }
        return Ok(());
    }
    if PERMISSIONS.iter().any(|(name, _)| *name == p) {
        Ok(())
    } else {
        let known: Vec<&str> = PERMISSIONS.iter().map(|(n, _)| *n).collect();
        Err(format!("unknown permission {p:?} (known: {})", known.join(", ")))
    }
}

pub fn parse(text: &str) -> Result<Manifest, String> {
    let doc = toml::parse(text)?;
    if let Some(t) = doc.tables.keys().next() {
        return Err(format!("unknown table [{t}]"));
    }
    let r = &doc.root;
    for k in r.keys() {
        if !KEYS.contains(&k.as_str()) {
            return Err(format!("unknown key `{k}` (known keys: {})", KEYS.join(", ")));
        }
    }
    let id = string(r, "id")?.ok_or("missing required key `id`")?;
    valid_id(&id)?;
    let version = string(r, "version")?.ok_or("missing required key `version`")?;
    if version.is_empty() || version.len() > 32 || !version.chars().all(|c| c.is_ascii_alphanumeric() || ".-+_~".contains(c)) {
        return Err(format!("bad `version` {version:?}: use letters, digits and . - + _ ~"));
    }
    let main = string(r, "main")?.ok_or("missing required key `main` (the app's class)")?;
    if !valid_class(&main) {
        return Err(format!("`main` must be a Java class name like org.example.Main, got {main:?}"));
    }
    let classpath = list(r, "classpath")?.unwrap_or_else(|| vec!["app.jar".into()]);
    if classpath.is_empty() {
        return Err("`classpath` must list at least one jar".into());
    }
    for c in &classpath {
        if c.starts_with('/') || c.split('/').any(|p| p == ".." || p.is_empty()) || c.contains(':') {
            return Err(format!("`classpath` entries are paths inside the package, got {c:?}"));
        }
    }
    let mut permissions = list(r, "permissions")?.unwrap_or_default();
    for p in &permissions {
        check_permission(p)?;
    }
    permissions.dedup();
    let memory_mb = match r.get("memory") {
        None => 64,
        Some(Value::Int(n)) if (16..=2048).contains(n) => *n as u32,
        Some(Value::Int(n)) => return Err(format!("`memory` must be 16-2048 (MiB), got {n}")),
        Some(v) => return Err(format!("`memory` must be an integer (MiB), got {}", v.type_name())),
    };
    let service = match r.get("service") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(v) => return Err(format!("`service` must be true or false, got {}", v.type_name())),
    };
    let restart = string(r, "restart")?.unwrap_or_else(|| "on-failure".into());
    if !["always", "on-failure", "never"].contains(&restart.as_str()) {
        return Err(format!("`restart` must be \"always\", \"on-failure\" or \"never\", got {restart:?}"));
    }
    let name = string(r, "name")?.unwrap_or_else(|| id.clone());
    let description = string(r, "description")?.unwrap_or_default();
    for (k, v) in [("name", &name), ("description", &description)] {
        if v.chars().any(|c| c.is_control()) || v.len() > 200 {
            return Err(format!("`{k}` must be one line of at most 200 bytes"));
        }
    }
    Ok(Manifest {
        id,
        name,
        version,
        description,
        main,
        classpath,
        args: list(r, "args")?.unwrap_or_default(),
        permissions,
        memory_mb,
        service,
        restart,
    })
}

impl Manifest {
    pub fn has(&self, perm: &str) -> bool {
        self.permissions.iter().any(|p| p == perm)
    }

    pub fn service_name(&self) -> String {
        format!("app.{}", self.id)
    }
}

/// A TOML basic string, for the files jrun writes.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn string(t: &Table, key: &str) -> Result<Option<String>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s.clone())),
        Some(v) => Err(format!("`{key}` must be a string, got {}", v.type_name())),
    }
}

fn list(t: &Table, key: &str) -> Result<Option<Vec<String>>, String> {
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

/// /etc/myos/jrun.toml: where the runtime is. Every key is optional.
///
/// ```toml
/// java = "/usr/lib/myos/jdk/bin/java"
/// classpath = ["/usr/share/java/myos-sdk.jar", "/usr/share/java/myos-bus.jar",
///              "/usr/share/java/protobuf-java.jar"]
/// java_options = []                # added to every app's JVM options
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    pub java: String,
    pub classpath: Vec<String>,
    pub java_options: Vec<String>,
}

pub const CONFIG_FILE: &str = "/etc/myos/jrun.toml";

impl Config {
    pub fn load() -> Result<Config, String> {
        let mut c = Config {
            java: "/usr/lib/myos/jdk/bin/java".into(),
            classpath: vec![
                "/usr/share/java/myos-sdk.jar".into(),
                "/usr/share/java/myos-bus.jar".into(),
                "/usr/share/java/protobuf-java.jar".into(),
            ],
            java_options: Vec::new(),
        };
        let text = match std::fs::read_to_string(CONFIG_FILE) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(c),
            Err(e) => return Err(format!("{CONFIG_FILE}: {e}")),
        };
        let doc = toml::parse(&text).map_err(|e| format!("{CONFIG_FILE}: {e}"))?;
        for k in doc.root.keys() {
            if !["java", "classpath", "java_options"].contains(&k.as_str()) {
                return Err(format!("{CONFIG_FILE}: unknown key `{k}`"));
            }
        }
        if let Some(j) = string(&doc.root, "java").map_err(|e| format!("{CONFIG_FILE}: {e}"))? {
            c.java = j;
        }
        if let Some(cp) = list(&doc.root, "classpath").map_err(|e| format!("{CONFIG_FILE}: {e}"))? {
            c.classpath = cp;
        }
        if let Some(o) = list(&doc.root, "java_options").map_err(|e| format!("{CONFIG_FILE}: {e}"))? {
            c.java_options = o;
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
id = "org.myos.hello"
version = "1.0.0"
main = "org.myos.hello.Hello"
permissions = ["log", "sysinfo", "call:app.org.myos.*"]
"#;

    #[test]
    fn minimal() {
        let m = parse(OK).unwrap();
        assert_eq!(m.classpath, vec!["app.jar"]);
        assert_eq!(m.memory_mb, 64);
        assert!(m.service);
        assert_eq!(m.name, "org.myos.hello");
        assert_eq!(bus_calls(&m.permissions), vec!["log", "sysinfo", "app.org.myos.*"]);
        assert!(!m.has("network"));
    }

    #[test]
    fn errors() {
        let bad = |extra: &str, want: &str| {
            let e = parse(&format!("{OK}{extra}")).unwrap_err();
            assert!(e.contains(want), "{extra}: {e}");
        };
        bad("memory = 4\n", "16-2048");
        bad("perms = []\n", "unknown key");
        bad("restart = \"sometimes\"\n", "restart");
        bad("classpath = [\"../x.jar\"]\n", "inside the package");
        bad("classpath = [\"/x.jar\"]\n", "inside the package");
        bad("[extra]\n", "unknown table");
        assert!(parse("id = \"x\"\nversion = \"1\"\nmain = \"A\"\n").unwrap_err().contains("bad app id"));
        assert!(parse(&OK.replace("\"sysinfo\"", "\"root\"")).unwrap_err().contains("unknown permission"));
        assert!(parse(&OK.replace("\"sysinfo\"", "\"call:bus\"")).is_err());
        assert!(parse(&OK.replace("org.myos.hello.Hello", "not a class")).is_err());
        assert!(parse(&OK.replace("1.0.0", "1 0")).is_err());
    }

    #[test]
    fn ids() {
        for ok in ["org.example.notes", "a.b", "com.x-y.z9"] {
            assert!(valid_id(ok).is_ok(), "{ok}");
        }
        for bad in ["hello", "Org.x", "a..b", "a.9b", "a.b_c", ".a.b", &format!("a.{}", "b".repeat(60))] {
            assert!(valid_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("a \"b\" \\ c"), r#""a \"b\" \\ c""#);
    }
}
