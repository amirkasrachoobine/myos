//! One system user per app, in /etc/passwd and /etc/group:
//!
//! ```text
//! app_hello:x:20001:20001:myos app org.myos.hello:/var/lib/myos/apps/org.myos.hello/data:/bin/false
//! app_hello:x:20001:
//! ```
//!
//! The comment field names the app, which is how jrun finds the user again.
//! The uid (and the gid, always the same number) comes from
//! APP_UID_MIN..=APP_UID_MAX, the range busd applies app policies to.

use myos_bus::{APP_UID_MAX, APP_UID_MIN};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub const PASSWD: &str = "/etc/passwd";
pub const GROUP: &str = "/etc/group";
const GECOS: &str = "myos app ";

#[derive(Debug, Clone, PartialEq)]
pub struct AppUser {
    pub name: String,
    pub uid: u32,
}

fn read(path: &str) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{path}: {e}")),
    }
}

/// Replaces a file's contents atomically, keeping its mode (0644 if new).
fn replace(path: &str, text: &str) -> Result<(), String> {
    let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644);
    let tmp = format!("{path}.jrun-tmp");
    let r = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    match r {
        // A bind-mounted file (containers, tests) can't be renamed over:
        // rewrite it in place instead.
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => std::fs::write(path, text).map_err(|e| format!("{path}: {e}")),
        r => r.map_err(|e| format!("{path}: {e}")),
    }
}

fn fields(line: &str) -> Vec<&str> {
    line.split(':').collect()
}

/// The user an app runs as, if it has one.
pub fn find(app_id: &str) -> Result<Option<AppUser>, String> {
    let want = format!("{GECOS}{app_id}");
    for line in read(PASSWD)?.lines() {
        let f = fields(line);
        if f.len() >= 7 && f[4] == want {
            if let Ok(uid) = f[2].parse::<u32>() {
                if (APP_UID_MIN..=APP_UID_MAX).contains(&uid) {
                    return Ok(Some(AppUser { name: f[0].to_string(), uid }));
                }
            }
        }
    }
    Ok(None)
}

/// "app_" + the id's last part, made unique.
fn user_name(app_id: &str, taken: &dyn Fn(&str) -> bool, uid: u32) -> String {
    let last = app_id.rsplit('.').next().unwrap_or(app_id);
    let mut base: String = last.chars().map(|c| if c == '-' { '_' } else { c }).collect();
    base.truncate(27);
    let name = format!("app_{base}");
    if !taken(&name) {
        return name;
    }
    format!("app_{}_{uid}", &base[..base.len().min(20)])
}

/// Finds or creates the app's user. Returns it and whether it's new.
pub fn ensure(app_id: &str, home: &Path) -> Result<(AppUser, bool), String> {
    if let Some(u) = find(app_id)? {
        return Ok((u, false));
    }
    let passwd = read(PASSWD)?;
    let group = read(GROUP)?;
    let mut used_ids = std::collections::BTreeSet::new();
    let mut names = std::collections::BTreeSet::new();
    for line in passwd.lines().chain(group.lines()) {
        let f = fields(line);
        if f.len() >= 3 {
            names.insert(f[0].to_string());
            if let Ok(n) = f[2].parse::<u32>() {
                used_ids.insert(n);
            }
        }
    }
    let uid = (APP_UID_MIN..=APP_UID_MAX)
        .find(|n| !used_ids.contains(n))
        .ok_or("no free app uids left")?;
    let name = user_name(app_id, &|n| names.contains(n), uid);
    let add = |text: &str, line: String| {
        let mut t = text.to_string();
        if !t.is_empty() && !t.ends_with('\n') {
            t.push('\n');
        }
        t.push_str(&line);
        t.push('\n');
        t
    };
    replace(
        PASSWD,
        &add(&passwd, format!("{name}:x:{uid}:{uid}:{GECOS}{app_id}:{}:/bin/false", home.display())),
    )?;
    replace(GROUP, &add(&group, format!("{name}:x:{uid}:")))?;
    Ok((AppUser { name, uid }, true))
}

/// Removes the app's user and group.
pub fn remove(user: &AppUser) -> Result<(), String> {
    let uid = user.uid.to_string();
    let drop_line = |text: &str, is_passwd: bool| -> String {
        text.lines()
            .filter(|l| {
                let f = fields(l);
                !(f.len() >= 3 && f[0] == user.name && f[2] == uid && (!is_passwd || f.len() >= 7))
            })
            .map(|l| format!("{l}\n"))
            .collect()
    };
    replace(PASSWD, &drop_line(&read(PASSWD)?, true))?;
    replace(GROUP, &drop_line(&read(GROUP)?, false))
}

#[cfg(test)]
mod tests {
    use super::user_name;

    #[test]
    fn names() {
        let none = |_: &str| false;
        assert_eq!(user_name("org.myos.hello", &none, 20001), "app_hello");
        assert_eq!(user_name("org.x.my-app", &none, 20001), "app_my_app");
        let all = |_: &str| true;
        assert_eq!(user_name("org.myos.hello", &all, 20002), "app_hello_20002");
        assert!(user_name(&format!("a.{}", "b".repeat(58)), &none, 1).len() <= 32);
    }
}
