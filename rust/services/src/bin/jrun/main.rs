//! jrun: installs and runs Java apps (Phase 6).
//!
//! An app package (.mapp) is a tar archive with app.toml (the manifest, see
//! manifest.rs) at the top and the app's jars. `jrun install` gives each app
//! its own user, a private data directory, a bus policy listing the services
//! its permissions allow, and a service file, so init runs it as the service
//! `app.<id>` and its output goes to logd. `jrun run` is what that service
//! runs: it sets up the sandbox (sandbox.rs) and starts the JVM.
//!
//! Layout:
//!   /var/lib/myos/apps/<id>/pkg/        the package contents (root, read-only to the app)
//!   /var/lib/myos/apps/<id>/data/       the app's own files (its uid, 0700)
//!   /etc/myos/bus/policy.d/<uid>        what it may call on the bus (busd reads it)
//!   /etc/myos/services/app.<id>.toml    its service file
//!   /etc/myos/jrun.toml                 where the JVM and the SDK are (optional)

mod manifest;
mod sandbox;
mod tar;
mod users;

use manifest::{quote, Config, Manifest};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const APPS_DIR: &str = "/var/lib/myos/apps";
const SERVICES_DIR: &str = "/etc/myos/services";

const USAGE: &str = "\
usage: jrun COMMAND ...

  install PACKAGE.mapp [--no-start]   install or upgrade an app (root)
  remove ID [--keep-data]             stop and uninstall it (root)
  list                                installed apps
  info ID                             an app's manifest, user, permissions and state
  run ID [ARGS...]                    run it in its sandbox, in the foreground (root;
                                      this is what the app's service runs)
  check PACKAGE.mapp|DIR              validate a package or a package directory
  pack DIR -o PACKAGE.mapp            make a package from a directory with app.toml
  permissions                         what apps may ask for

Apps run as services named app.<ID>: myctl status|start|stop|logs app.<ID>.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("install") => cmd_install(&args[1..]),
        Some("remove" | "uninstall") => cmd_remove(&args[1..]),
        Some("list" | "ls") if args.len() == 1 => cmd_list(),
        Some("info") if args.len() == 2 => cmd_info(&args[1]),
        Some("run") if args.len() >= 2 => cmd_run(&args[1], &args[2..]),
        Some("check") if args.len() == 2 => cmd_check(&args[1]),
        Some("pack") => cmd_pack(&args[1..]),
        Some("permissions") => {
            for (p, what) in manifest::PERMISSIONS {
                println!("{p:<14} {what}");
            }
            Ok(())
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--version") => {
            println!("jrun {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("jrun: {e}");
            ExitCode::FAILURE
        }
    }
}

fn need_root(what: &str) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err(format!("{what} needs root"));
    }
    Ok(())
}

fn app_dir(id: &str) -> PathBuf {
    Path::new(APPS_DIR).join(id)
}

fn policy_file(uid: u32) -> PathBuf {
    Path::new(myos_bus::DEFAULT_POLICY_DIR).join(uid.to_string())
}

fn service_file(id: &str) -> PathBuf {
    Path::new(SERVICES_DIR).join(format!("app.{id}.toml"))
}

/// The manifest of an installed app.
fn installed(id: &str) -> Result<Manifest, String> {
    manifest::valid_id(id)?;
    let path = app_dir(id).join("pkg/app.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("{id} isn't installed")),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    manifest::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

// ---- packages -------------------------------------------------------------

/// Checks a package's contents; returns its manifest.
fn validate(entries: &tar::Entries) -> Result<Manifest, String> {
    let text = match entries.get("app.toml") {
        Some(tar::Item::File { data, .. }) => {
            String::from_utf8(data.clone()).map_err(|_| "app.toml isn't UTF-8".to_string())?
        }
        _ => return Err("the package has no app.toml at its top level".into()),
    };
    let m = manifest::parse(&text).map_err(|e| format!("app.toml: {e}"))?;
    for jar in &m.classpath {
        match entries.get(jar) {
            Some(tar::Item::File { .. }) => {}
            _ => return Err(format!("app.toml: classpath entry {jar:?} isn't in the package")),
        }
    }
    Ok(m)
}

/// A package directory as archive entries: regular files and directories
/// only, no symlinks.
fn read_dir(dir: &Path) -> Result<tar::Entries, String> {
    fn walk(base: &Path, rel: &str, out: &mut tar::Entries) -> Result<(), String> {
        let path = if rel.is_empty() { base.to_path_buf() } else { base.join(rel) };
        let mut names: Vec<_> = std::fs::read_dir(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .filter_map(|e| e.ok())
            .collect();
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let name = e.file_name().into_string().map_err(|n| format!("{n:?}: file name isn't UTF-8"))?;
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let meta = std::fs::symlink_metadata(base.join(&r)).map_err(|e| format!("{r}: {e}"))?;
            if meta.is_dir() {
                out.insert(r.clone(), tar::Item::Dir);
                walk(base, &r, out)?;
            } else if meta.is_file() {
                let data = std::fs::read(base.join(&r)).map_err(|e| format!("{r}: {e}"))?;
                out.insert(r, tar::Item::File { data, executable: meta.mode() & 0o111 != 0 });
            } else {
                return Err(format!("{r}: only files and directories can go in a package"));
            }
        }
        Ok(())
    }
    let mut out = tar::Entries::new();
    walk(dir, "", &mut out)?;
    Ok(out)
}

fn load_package(path: &str) -> Result<tar::Entries, String> {
    let p = Path::new(path);
    if p.is_dir() {
        return read_dir(p);
    }
    let meta = std::fs::metadata(p).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > tar::MAX_TOTAL + (16 << 20) {
        return Err(format!("{path}: too large for an app package"));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("{path}: {e}"))?;
    tar::read(&bytes).map_err(|e| format!("{path}: {e}"))
}

fn cmd_check(path: &str) -> Result<(), String> {
    let entries = load_package(path)?;
    let m = validate(&entries).map_err(|e| format!("{path}: {e}"))?;
    let files = entries.values().filter(|i| matches!(i, tar::Item::File { .. })).count();
    println!("ok  {} {} ({files} files)", m.id, m.version);
    println!("    main {}  classpath {}", m.main, m.classpath.join(":"));
    println!("    permissions: {}", if m.permissions.is_empty() { "none".into() } else { m.permissions.join(", ") });
    Ok(())
}

fn cmd_pack(args: &[String]) -> Result<(), String> {
    let (dir, out) = match args {
        [d, o, f] if o == "-o" => (d, f),
        [o, f, d] if o == "-o" => (d, f),
        _ => return Err("usage: jrun pack DIR -o PACKAGE.mapp".into()),
    };
    let entries = read_dir(Path::new(dir))?;
    let m = validate(&entries).map_err(|e| format!("{dir}: {e}"))?;
    let bytes = tar::write(&entries, 0)?;
    std::fs::write(out, bytes).map_err(|e| format!("{out}: {e}"))?;
    println!("{out}: {} {}", m.id, m.version);
    Ok(())
}

// ---- talking to init ------------------------------------------------------

fn init_socket() -> String {
    std::env::var("MYOS_INIT_SOCKET").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/run/myos/init.sock".into())
}

/// One initctl request. None if init can't be reached (not running myos-init,
/// e.g. while building an image).
fn init(request: &str) -> Option<Result<String, String>> {
    let mut s = UnixStream::connect(init_socket()).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reply = String::new();
    let r = s
        .write_all(format!("{request}\n").as_bytes())
        .and_then(|_| s.shutdown(std::net::Shutdown::Write))
        .and_then(|_| s.read_to_string(&mut reply));
    Some(match r {
        Err(e) => Err(format!("init: {e}")),
        Ok(_) => match reply.strip_prefix("error:") {
            Some(e) => Err(e.trim().to_string()),
            None => Ok(reply),
        },
    })
}

/// The service's state word from `status` ("running", "stopped", ...).
fn service_state(name: &str) -> Option<String> {
    match init(&format!("status {name}"))? {
        Ok(line) => line.split_whitespace().nth(1).map(str::to_string),
        Err(_) => None,
    }
}

fn status_line(name: &str) -> Option<String> {
    init(&format!("status {name}"))?.ok().map(|s| s.trim_end().to_string())
}

// ---- install / remove -----------------------------------------------------

struct Lock(#[allow(dead_code)] std::fs::File);

fn lock() -> Result<Lock, String> {
    std::fs::create_dir_all(APPS_DIR).map_err(|e| format!("{APPS_DIR}: {e}"))?;
    let path = Path::new(APPS_DIR).join(".lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    use std::os::fd::AsRawFd;
    // SAFETY: flock on an open fd.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error()));
    }
    Ok(Lock(f))
}

fn mkdir(path: &Path, mode: u32, owner: Option<u32>) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{}: exists and isn't a directory", path.display())),
        Err(_) => std::fs::create_dir_all(path).map_err(err)?,
    }
    if let Some(id) = owner {
        std::os::unix::fs::chown(path, Some(id), Some(id)).map_err(err)?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(err)
}

fn write_file(path: &Path, text: &str, mode: u32) -> Result<(), String> {
    let tmp = path.with_extension("jrun-tmp");
    let r = std::fs::write(&tmp, text)
        .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)))
        .and_then(|_| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(|e| format!("{}: {e}", path.display()))
}

fn extract(entries: &tar::Entries, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    mkdir(dest, 0o755, None)?;
    for (path, item) in entries {
        let p = dest.join(path);
        match item {
            tar::Item::Dir => mkdir(&p, 0o755, None)?,
            tar::Item::File { data, executable } => {
                std::fs::write(&p, data)
                    .and_then(|_| std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if *executable { 0o755 } else { 0o644 })))
                    .map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
    }
    Ok(())
}

fn policy_text(m: &Manifest) -> String {
    let mut s = format!("# Bus policy for app {} {}, written by jrun install. busd reads it.\napp {}\n", m.id, m.version, m.id);
    for c in manifest::bus_calls(&m.permissions) {
        s.push_str(&format!("call {c}\n"));
    }
    s
}

/// The jrun the service should run: this one (/usr/bin/jrun on the system).
fn self_path() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "/usr/bin/jrun".into())
}

fn service_text(m: &Manifest, enabled: bool) -> String {
    let what = if m.name == m.id { m.id.clone() } else { format!("{} ({})", m.name, m.id) };
    format!(
        "# Java app {id} {version}, written by jrun install; jrun remove deletes it.\n\
         # Changes here are lost when the app is upgraded, except `enabled`.\n\
         description = {desc}\n\
         exec = [{jrun}, \"run\", {qid}]\n\
         restart = {restart}\n\
         after = [\"busd\", \"logd\"]\n\
         enabled = {enabled}\n\
         stop_timeout = 10\n",
        id = m.id,
        version = m.version,
        desc = quote(&format!("Java app {what}")),
        qid = quote(&m.id),
        jrun = quote(&self_path()),
        restart = quote(&m.restart),
    )
}

/// `enabled` from an existing service file (so an upgrade keeps it).
fn was_enabled(path: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "enabled").then(|| v.split('#').next().unwrap_or("").trim() == "true")
    })
}

/// Waits until the service has settled (running, failed, ...) for up to
/// `secs`; returns its status line.
fn settle(name: &str, secs: u64) -> Option<String> {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let st = service_state(name)?;
        if !matches!(st.as_str(), "waiting" | "stopping") || Instant::now() >= end {
            return status_line(name);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn cmd_install(args: &[String]) -> Result<(), String> {
    let mut path = None;
    let mut no_start = false;
    for a in args {
        match a.as_str() {
            "--no-start" => no_start = true,
            a if a.starts_with('-') => return Err(format!("unknown option {a}")),
            a if path.is_none() => path = Some(a.to_string()),
            _ => return Err("usage: jrun install PACKAGE.mapp [--no-start]".into()),
        }
    }
    let path = path.ok_or("usage: jrun install PACKAGE.mapp [--no-start]")?;
    need_root("jrun install")?;
    let entries = load_package(&path)?;
    let m = validate(&entries).map_err(|e| format!("{path}: {e}"))?;
    let _lock = lock()?;

    let dir = app_dir(&m.id);
    let old = installed(&m.id).ok();
    let svc = m.service_name();
    let was_running = old.is_some() && service_state(&svc).as_deref() == Some("running");

    mkdir(Path::new(APPS_DIR), 0o755, None)?;
    mkdir(&dir, 0o755, None)?;
    let data = dir.join("data");
    let (user, new_user) = users::ensure(&m.id, &data)?;
    mkdir(&data, 0o700, Some(user.uid))?;

    // New contents next to the old, then swap.
    let staged = dir.join(".pkg.new");
    extract(&entries, &staged)?;
    let pkg = dir.join("pkg");
    let retired = dir.join(".pkg.old");
    if retired.exists() {
        std::fs::remove_dir_all(&retired).map_err(|e| format!("{}: {e}", retired.display()))?;
    }
    if pkg.exists() {
        std::fs::rename(&pkg, &retired).map_err(|e| format!("{}: {e}", pkg.display()))?;
    }
    std::fs::rename(&staged, &pkg).map_err(|e| format!("{}: {e}", pkg.display()))?;
    let _ = std::fs::remove_dir_all(&retired);

    mkdir(Path::new(myos_bus::DEFAULT_POLICY_DIR), 0o755, None)?;
    write_file(&policy_file(user.uid), &policy_text(&m), 0o644)?;

    let sfile = service_file(&m.id);
    let enabled = if no_start { false } else { was_enabled(&sfile).unwrap_or(true) };
    if m.service {
        mkdir(Path::new(SERVICES_DIR), 0o755, None)?;
        write_file(&sfile, &service_text(&m, enabled), 0o644)?;
    } else if sfile.exists() {
        let _ = init(&format!("stop {svc}"));
        std::fs::remove_file(&sfile).map_err(|e| format!("{}: {e}", sfile.display()))?;
    }

    match &old {
        Some(o) => println!("upgraded {} {} -> {}", m.id, o.version, m.version),
        None => println!("installed {} {}", m.id, m.version),
    }
    println!(
        "  user {} (uid {}){}, data in {}",
        user.name,
        user.uid,
        if new_user { ", new" } else { "" },
        data.display()
    );
    println!("  permissions: {}", if m.permissions.is_empty() { "none".into() } else { m.permissions.join(", ") });

    let reload = init("reload");
    match reload {
        None if m.service => println!("  service {svc}: init isn't running; it starts at the next boot"),
        None => {}
        Some(Err(e)) => return Err(format!("installed, but init's reload failed: {e}")),
        Some(Ok(_)) if !m.service => println!("  not a service: start it with `jrun run {}`", m.id),
        Some(Ok(_)) if !enabled => println!("  service {svc}: disabled; `myctl enable {svc}` to start it"),
        Some(Ok(_)) => {
            if was_running {
                if let Some(Err(e)) = init(&format!("restart {svc}")) {
                    return Err(format!("installed, but restarting {svc} failed: {e}"));
                }
            }
            match settle(&svc, 5) {
                Some(line) => println!("  service {}", line.trim()),
                None => println!("  service {svc}: started"),
            }
        }
    }
    Ok(())
}

fn cmd_remove(args: &[String]) -> Result<(), String> {
    let (id, keep_data) = match args {
        [id] => (id, false),
        [id, k] | [k, id] if k == "--keep-data" => (id, true),
        _ => return Err("usage: jrun remove ID [--keep-data]".into()),
    };
    manifest::valid_id(id)?;
    need_root("jrun remove")?;
    let _lock = lock()?;
    let dir = app_dir(id);
    let user = users::find(id)?;
    if !dir.exists() && user.is_none() {
        return Err(format!("{id} isn't installed"));
    }
    let svc = format!("app.{id}");
    let sfile = service_file(id);
    if sfile.exists() {
        if let Some(Err(e)) = init(&format!("stop {svc}")) {
            if !e.contains("no service") && !e.contains("unknown") {
                eprintln!("jrun: stopping {svc}: {e}");
            }
        }
        std::fs::remove_file(&sfile).map_err(|e| format!("{}: {e}", sfile.display()))?;
        let _ = init("reload");
    }
    if let Some(u) = &user {
        sandbox::kill_all(u.uid);
        let pf = policy_file(u.uid);
        if pf.exists() {
            std::fs::remove_file(&pf).map_err(|e| format!("{}: {e}", pf.display()))?;
        }
    }
    if keep_data {
        let pkg = dir.join("pkg");
        if pkg.exists() {
            std::fs::remove_dir_all(&pkg).map_err(|e| format!("{}: {e}", pkg.display()))?;
        }
        println!("removed {id}; kept {} and its user for a reinstall", dir.join("data").display());
    } else {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        if let Some(u) = &user {
            users::remove(u)?;
        }
        println!("removed {id}");
    }
    Ok(())
}

// ---- list / info ----------------------------------------------------------

fn ids() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(APPS_DIR)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.') && app_dir(n).join("pkg/app.toml").exists())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn cmd_list() -> Result<(), String> {
    let ids = ids();
    if ids.is_empty() {
        println!("no apps installed (jrun install PACKAGE.mapp)");
        return Ok(());
    }
    println!("{:<32} {:<10} {:<16} STATE", "ID", "VERSION", "USER");
    for id in ids {
        let (version, svc) = match installed(&id) {
            Ok(m) => (m.version.clone(), m.service),
            Err(_) => ("?".into(), false),
        };
        let user = users::find(&id).ok().flatten().map(|u| u.name).unwrap_or_else(|| "?".into());
        let state = if !svc {
            "not a service".into()
        } else {
            service_state(&format!("app.{id}")).unwrap_or_else(|| "-".into())
        };
        println!("{id:<32} {version:<10} {user:<16} {state}");
    }
    Ok(())
}

fn cmd_info(id: &str) -> Result<(), String> {
    let m = installed(id)?;
    let user = users::find(id)?;
    println!("id           {}", m.id);
    println!("name         {}", m.name);
    println!("version      {}", m.version);
    if !m.description.is_empty() {
        println!("description  {}", m.description);
    }
    println!("main         {}", m.main);
    println!("classpath    {}", m.classpath.join(":"));
    println!("memory       {} MiB heap", m.memory_mb);
    match &user {
        Some(u) => println!("user         {} (uid {})", u.name, u.uid),
        None => println!("user         missing! reinstall the app"),
    }
    println!("files        {}", app_dir(id).join("pkg").display());
    println!("data         {}", app_dir(id).join("data").display());
    println!("permissions  {}", if m.permissions.is_empty() { "none".to_string() } else { m.permissions.join(", ") });
    println!("bus calls    bus, app.{}{}", m.id, manifest::bus_calls(&m.permissions).iter().map(|c| format!(", {c}")).collect::<String>());
    println!("network      {}", if m.has("network") { "yes" } else { "loopback only (own namespace)" });
    if m.service {
        let svc = m.service_name();
        println!("service      {}", status_line(&svc).unwrap_or_else(|| format!("{svc} (state unknown: needs root and myos-init)")));
    } else {
        println!("service      no (run it with `jrun run {id}`)");
    }
    Ok(())
}

// ---- run ------------------------------------------------------------------

fn cmd_run(id: &str, extra: &[String]) -> Result<(), String> {
    need_root("jrun run (it switches to the app's own user)")?;
    let m = installed(id)?;
    let user = users::find(id)?.ok_or_else(|| format!("{id} has no user; reinstall it"))?;
    let config = Config::load()?;
    let dir = app_dir(id);
    let pkg = dir.join("pkg");
    let data = dir.join("data");
    mkdir(&data, 0o700, Some(user.uid))?;
    if !Path::new(&config.java).exists() {
        return Err(format!("{}: no Java runtime there (set `java` in {})", config.java, manifest::CONFIG_FILE));
    }

    let mut cp: Vec<String> = config.classpath.clone();
    cp.extend(m.classpath.iter().map(|j| pkg.join(j).display().to_string()));
    let mut argv = vec![
        config.java.clone(),
        format!("-Xmx{}m", m.memory_mb),
        "-XX:+UseSerialGC".into(),
        "-XX:TieredStopAtLevel=1".into(),
        "-XX:-UsePerfData".into(),
        "-Xshare:auto".into(),
        "-Djava.io.tmpdir=/tmp".into(),
        "-Dfile.encoding=UTF-8".into(),
        format!("-Dmyos.app.id={}", m.id),
        format!("-Dmyos.app.name={}", m.name),
        format!("-Dmyos.app.version={}", m.version),
        format!("-Dmyos.app.main={}", m.main),
        format!("-Dmyos.app.home={}", pkg.display()),
        format!("-Dmyos.app.data={}", data.display()),
        format!("-Dmyos.app.permissions={}", m.permissions.join(",")),
    ];
    argv.extend(config.java_options.iter().cloned());
    argv.push("-cp".into());
    argv.push(cp.join(":"));
    argv.push("org.myos.runtime.Main".into());
    argv.extend(m.args.iter().cloned());
    argv.extend(extra.iter().cloned());

    let mut env = vec![
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("HOME".to_string(), data.display().to_string()),
        ("USER".to_string(), user.name.clone()),
        ("LOGNAME".to_string(), user.name.clone()),
        ("TMPDIR".to_string(), "/tmp".to_string()),
        ("MYOS_APP_ID".to_string(), m.id.clone()),
    ];
    for k in ["LANG", "TZ", "TERM", "MYOS_BUS_SOCKET"] {
        if let Ok(v) = std::env::var(k) {
            env.push((k.to_string(), v));
        }
    }
    if !env.iter().any(|(k, _)| k == "LANG") {
        env.push(("LANG".into(), "C.UTF-8".into()));
    }
    let plan = sandbox::Plan { uid: user.uid, network: m.has("network"), data, argv, env };
    sandbox::exec(&plan).map(|_| ())
}
