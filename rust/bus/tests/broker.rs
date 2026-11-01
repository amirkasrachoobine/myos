//! End-to-end tests: start the real busd on a temporary socket and talk to
//! it through the library.

use myos_bus::{proto, reply, Client, Code, Error, Service, Status};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Busd {
    child: Child,
    dir: PathBuf,
    sock: PathBuf,
}

impl Busd {
    fn start(tag: &str) -> Busd {
        let dir = std::env::temp_dir().join(format!("myos-bus-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("run/bus.sock"); // busd must create run/
        let child = Command::new(env!("CARGO_BIN_EXE_busd"))
            .arg("--socket")
            .arg(&sock)
            .stderr(Stdio::null())
            .spawn()
            .expect("start busd");
        let t0 = Instant::now();
        while Client::connect_to(&sock).is_err() {
            assert!(t0.elapsed() < Duration::from_secs(5), "busd didn't come up");
            std::thread::sleep(Duration::from_millis(20));
        }
        Busd { child, dir, sock }
    }
    fn client(&self) -> Client {
        Client::connect_to(&self.sock).unwrap()
    }
}

impl Drop for Busd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A service that echoes Ping and can be told to hang or die.
fn spawn_echo(sock: &Path, name: &'static str) {
    let svc = Service::register_at(sock, &[name]).unwrap();
    std::thread::spawn(move || {
        let _ = svc.serve(|req| match req.method {
            "Ping" => {
                let r: proto::PingRequest = req.decode()?;
                reply(&proto::PingReply {
                    message: format!("{} says {}", req.service, r.message),
                    caller: Some(req.caller),
                    ..Default::default()
                })
            }
            "Hang" => {
                std::thread::sleep(Duration::from_secs(3));
                Ok(Vec::new())
            }
            "Die" => std::process::exit(99), // not used in-process
            "Panic" => panic!("boom"),
            m => Err(Status::unknown_method(m)),
        });
    });
}

#[test]
fn ping_reports_caller() {
    let b = Busd::start("ping");
    let r = b.client().ping("hi").unwrap();
    assert_eq!(r.message, "hi");
    assert!(r.broker_version.starts_with("busd "));
    let me = r.caller.unwrap();
    assert_eq!(me.pid as u32, std::process::id());
    assert_eq!(me.uid, unsafe { libc::getuid() });
}

#[test]
fn routes_calls_to_a_registered_service() {
    let b = Busd::start("route");
    spawn_echo(&b.sock, "app.test.echo");
    let mut c = b.client();
    let names: Vec<String> = c.list_services().unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["bus", "app.test.echo"]);

    let r: proto::PingReply = c.call("app.test.echo", "Ping", &proto::PingRequest { message: "yo".into() }).unwrap();
    assert_eq!(r.message, "app.test.echo says yo");
    assert_eq!(r.caller.unwrap().pid as u32, std::process::id(), "broker fills in the caller");

    let e = c.call_raw("app.test.echo", "Nope", vec![]).unwrap_err();
    assert_eq!(e.code(), Some(Code::UnknownMethod));
    let e = c.call_raw("app.test.echo", "Panic", vec![]).unwrap_err();
    assert_eq!(e.code(), Some(Code::Internal));
    let e = c.call_raw("app.test.echo", "Ping", vec![0xff, 0xff]).unwrap_err();
    assert_eq!(e.code(), Some(Code::BadRequest));
    // Status errors leave the connection usable.
    assert_eq!(c.ping("still here").unwrap().message, "still here");
}

#[test]
fn many_concurrent_callers() {
    let b = Busd::start("concurrent");
    spawn_echo(&b.sock, "app.test.echo");
    let sock = b.sock.clone();
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let sock = sock.clone();
            std::thread::spawn(move || {
                let mut c = Client::connect_to(&sock).unwrap();
                for i in 0..200 {
                    let msg = format!("{t}-{i}");
                    let r: proto::PingReply =
                        c.call("app.test.echo", "Ping", &proto::PingRequest { message: msg.clone() }).unwrap();
                    assert_eq!(r.message, format!("app.test.echo says {msg}"));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

#[test]
fn unknown_service_and_bad_names() {
    let b = Busd::start("unknown");
    let e = b.client().call_raw("nobody", "Ping", vec![]).unwrap_err();
    assert_eq!(e.code(), Some(Code::UnknownService));
    assert_eq!(b.client().call_raw("bus", "Nope", vec![]).unwrap_err().code(), Some(Code::UnknownMethod));

    let code = |r: Result<Service, Error>| r.err().and_then(|e| e.code());
    assert_eq!(code(Service::register_at(&b.sock, &["Bad Name"])), Some(Code::BadRequest));
    assert_eq!(code(Service::register_at(&b.sock, &["bus"])), Some(Code::PermissionDenied));
    if unsafe { libc::getuid() } != 0 {
        assert_eq!(code(Service::register_at(&b.sock, &["logd"])), Some(Code::PermissionDenied));
    }
}

#[test]
fn names_are_exclusive_and_freed_on_disconnect() {
    let b = Busd::start("exclusive");
    let first = Service::register_at(&b.sock, &["app.test.one"]).unwrap();
    let e = Service::register_at(&b.sock, &["app.test.one"]).err().unwrap();
    assert_eq!(e.code(), Some(Code::AlreadyRegistered));
    drop(first);
    let t0 = Instant::now();
    loop {
        match Service::register_at(&b.sock, &["app.test.one"]) {
            Ok(_) => break,
            Err(e) if t0.elapsed() < Duration::from_secs(2) => {
                assert_eq!(e.code(), Some(Code::AlreadyRegistered));
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("name not freed: {e}"),
        }
    }
}

#[test]
fn slow_service_times_out_and_dead_service_is_unavailable() {
    let b = Busd::start("timeout");
    spawn_echo(&b.sock, "app.test.slow");
    let mut c = b.client();
    c.set_timeout(Duration::from_millis(300));
    let t0 = Instant::now();
    let e = c.call_raw("app.test.slow", "Hang", vec![]).unwrap_err();
    assert_eq!(e.code(), Some(Code::Timeout), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(1));

    // A service connection that dies with a call in flight: the caller
    // gets UNAVAILABLE right away rather than waiting for the timeout.
    let svc = Service::register_at(&b.sock, &["app.test.dies"]).unwrap();
    let sock = b.sock.clone();
    let caller = std::thread::spawn(move || {
        let mut c = Client::connect_to(&sock).unwrap();
        c.set_timeout(Duration::from_secs(10));
        let t0 = Instant::now();
        (c.call_raw("app.test.dies", "Anything", vec![]).unwrap_err().code(), t0.elapsed())
    });
    std::thread::sleep(Duration::from_millis(300));
    drop(svc);
    let (code, took) = caller.join().unwrap();
    assert_eq!(code, Some(Code::Unavailable));
    assert!(took < Duration::from_secs(2));
}

#[test]
fn refuses_to_start_twice_and_replaces_stale_socket() {
    let mut b = Busd::start("twice");
    let out = Command::new(env!("CARGO_BIN_EXE_busd")).arg("--socket").arg(&b.sock).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already listening"));
    assert!(b.client().ping("x").is_ok(), "the first broker is unaffected");

    // Kill it hard: the socket file stays behind, and a new busd must cope.
    b.child.kill().unwrap();
    b.child.wait().unwrap();
    assert!(b.sock.exists());
    let again = Busd::start_at(b.dir.clone(), b.sock.clone());
    assert_eq!(again.client().ping("back").unwrap().message, "back");
}

#[test]
fn service_waits_for_a_late_broker() {
    let dir = std::env::temp_dir().join(format!("myos-bus-test-{}-late", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sock = dir.join("bus.sock");
    let s2 = sock.clone();
    let svc = std::thread::spawn(move || Service::register_at(&s2, &["app.test.late"]).map(|_| ()));
    std::thread::sleep(Duration::from_millis(500));
    let b = Busd::start_at(dir, sock);
    svc.join().unwrap().expect("registered once busd came up");
    drop(b);
}

impl Busd {
    fn start_at(dir: PathBuf, sock: PathBuf) -> Busd {
        let child = Command::new(env!("CARGO_BIN_EXE_busd"))
            .arg("--socket")
            .arg(&sock)
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let t0 = Instant::now();
        while Client::connect_to(&sock).is_err() {
            assert!(t0.elapsed() < Duration::from_secs(5), "busd didn't come up");
            std::thread::sleep(Duration::from_millis(20));
        }
        Busd { child, dir, sock }
    }
}
