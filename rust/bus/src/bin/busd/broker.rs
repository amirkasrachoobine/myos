//! The routing core: who owns which service name, and which forwarded calls
//! are still waiting for a reply.
//!
//! One thread per connection reads frames and dispatches them. Writes to a
//! connection go through that connection's writer lock, never while the
//! state lock is held, so a slow peer can only stall the thread writing to
//! it (and only up to the socket's write timeout, after which it's dropped).

use myos_bus::frame::{read_frame, write_frame};
use myos_bus::proto::{self, frame::Kind, Call, Caller, Frame, Register, Reply};
use myos_bus::{Code, Status, BUS_SERVICE};
use prost::Message;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::log;
use crate::policy::{is_app_uid, Policies};

const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SWEEP_EVERY: Duration = Duration::from_millis(200);
const VERSION: &str = concat!("busd ", env!("CARGO_PKG_VERSION"));

pub struct Conn {
    id: u64,
    cred: Caller,
    writer: Mutex<UnixStream>,
}

impl Conn {
    /// Sends a frame. On failure the connection is shut down, which makes
    /// its reader thread clean up; returns false.
    fn send(&self, frame: &Frame) -> bool {
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        match write_frame(&mut *w, frame) {
            Ok(()) => true,
            Err(e) => {
                log!("dropping connection from pid {}: write failed: {e}", self.cred.pid);
                let _ = w.shutdown(Shutdown::Both);
                false
            }
        }
    }

    fn reply(&self, id: u64, status: Status, payload: Vec<u8>) -> bool {
        self.send(&Frame { id, kind: Some(Kind::Reply(Reply { status: Some(status), payload })) })
    }

    fn fail(&self, id: u64, code: Code, message: impl Into<String>) {
        self.reply(id, Status::new(code, message), Vec::new());
    }
}

struct Pending {
    caller: Arc<Conn>,
    caller_id: u64,
    target: u64,
    deadline: Instant,
    what: String, // "service.Method", for messages
}

#[derive(Default)]
struct State {
    services: BTreeMap<String, Arc<Conn>>,
    pending: HashMap<u64, Pending>,
    next_call: u64,
}

pub struct Broker {
    state: Mutex<State>,
    started: Instant,
    next_conn: AtomicU64,
    verbose: bool,
    policies: Policies,
}

impl Broker {
    pub fn new(verbose: bool, policies: Policies) -> Arc<Broker> {
        Arc::new(Broker {
            state: Mutex::new(State::default()),
            started: Instant::now(),
            next_conn: AtomicU64::new(1),
            verbose,
            policies,
        })
    }

    /// Phase 6: may this connection call `service`? Err is the reason.
    fn check_call(&self, conn: &Conn, service: &str) -> Result<(), String> {
        if !is_app_uid(conn.cred.uid) || service == BUS_SERVICE {
            return Ok(());
        }
        let p = self.policies.get(conn.cred.uid)?;
        if p.may_call(service) {
            Ok(())
        } else {
            Err(format!("app {} (uid {}) may not call {service:?}: its manifest doesn't ask for it", p.app, conn.cred.uid))
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Accepts connections forever.
    pub fn serve(self: &Arc<Self>, listener: UnixListener) -> io::Error {
        let sweeper = Arc::clone(self);
        std::thread::Builder::new()
            .name("sweeper".into())
            .spawn(move || sweeper.sweep_forever())
            .expect("spawn sweeper thread");
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let me = Arc::clone(self);
                    if let Err(e) = std::thread::Builder::new().name("conn".into()).spawn(move || me.handle(stream)) {
                        log!("can't start a connection thread: {e}");
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_transient_accept_error(&e) => {
                    log!("accept: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return e,
            }
        }
    }

    fn handle(self: Arc<Self>, mut stream: UnixStream) {
        let cred = match peer_cred(&stream) {
            Ok(c) => c,
            Err(e) => {
                log!("rejecting a connection: can't read its credentials: {e}");
                return;
            }
        };
        let writer = match stream.set_write_timeout(Some(WRITE_TIMEOUT)).and_then(|_| stream.try_clone()) {
            Ok(w) => w,
            Err(e) => {
                log!("rejecting a connection: {e}");
                return;
            }
        };
        let conn = Arc::new(Conn {
            id: self.next_conn.fetch_add(1, Ordering::Relaxed),
            cred,
            writer: Mutex::new(writer),
        });
        if self.verbose {
            log!("connection {} from pid {} uid {}", conn.id, cred.pid, cred.uid);
        }
        loop {
            match read_frame(&mut stream) {
                Ok(Some(frame)) => self.dispatch(&conn, frame),
                Ok(None) => break,
                Err(e) => {
                    if e.kind() != io::ErrorKind::ConnectionReset {
                        log!("dropping connection from pid {}: {e}", cred.pid);
                    }
                    break;
                }
            }
        }
        self.disconnect(&conn);
    }

    fn dispatch(&self, conn: &Arc<Conn>, frame: Frame) {
        match frame.kind {
            Some(Kind::Call(call)) => self.on_call(conn, frame.id, call),
            Some(Kind::Reply(reply)) => self.on_reply(conn, frame.id, reply),
            Some(Kind::Register(reg)) => self.on_register(conn, frame.id, reg),
            None => conn.fail(frame.id, Code::BadRequest, "frame has no call, reply or register"),
        }
    }

    fn on_call(&self, conn: &Arc<Conn>, id: u64, mut call: Call) {
        if let Err(why) = self.check_call(conn, &call.service) {
            if self.verbose {
                log!("denied: {why}");
            }
            return conn.fail(id, Code::PermissionDenied, why);
        }
        if call.service == BUS_SERVICE {
            match self.builtin(conn, &call) {
                Ok(payload) => conn.reply(id, Status::ok(), payload),
                Err(status) => conn.reply(id, status, Vec::new()),
            };
            return;
        }
        let timeout = match call.timeout_ms {
            0 => DEFAULT_CALL_TIMEOUT,
            ms => Duration::from_millis(ms as u64),
        };
        let what = format!("{}.{}", call.service, call.method);
        let (target, forward_id) = {
            let mut st = self.state();
            let Some(target) = st.services.get(&call.service).cloned() else {
                drop(st);
                conn.fail(id, Code::UnknownService, format!("no service {:?} on the bus", call.service));
                return;
            };
            st.next_call += 1;
            let forward_id = st.next_call;
            st.pending.insert(
                forward_id,
                Pending {
                    caller: Arc::clone(conn),
                    caller_id: id,
                    target: target.id,
                    deadline: Instant::now() + timeout,
                    what: what.clone(),
                },
            );
            (target, forward_id)
        };
        call.caller = Some(conn.cred);
        if !target.send(&Frame { id: forward_id, kind: Some(Kind::Call(call)) }) {
            // The target's reader thread will also clean up, but answer now.
            if self.state().pending.remove(&forward_id).is_some() {
                conn.fail(id, Code::Unavailable, format!("{what}: the service went away"));
            }
        }
    }

    fn on_reply(&self, conn: &Arc<Conn>, id: u64, reply: Reply) {
        let pending = {
            let mut st = self.state();
            match st.pending.get(&id) {
                Some(p) if p.target == conn.id => st.pending.remove(&id),
                _ => None, // late (already timed out), or not this connection's to answer
            }
        };
        if let Some(p) = pending {
            p.caller.send(&Frame { id: p.caller_id, kind: Some(Kind::Reply(reply)) });
        }
    }

    fn on_register(&self, conn: &Arc<Conn>, id: u64, reg: Register) {
        let name = reg.service;
        if let Err(why) = valid_name(&name) {
            return conn.fail(id, Code::BadRequest, why);
        }
        if name == BUS_SERVICE {
            return conn.fail(id, Code::PermissionDenied, "\"bus\" is the broker's own name");
        }
        if conn.cred.uid != 0 && !name.starts_with("app.") {
            return conn.fail(id, Code::PermissionDenied, "only root may register names outside \"app.\"");
        }
        if is_app_uid(conn.cred.uid) {
            match self.policies.get(conn.cred.uid) {
                Ok(p) if p.may_register(&name) => {}
                Ok(p) => {
                    return conn.fail(
                        id,
                        Code::PermissionDenied,
                        format!("app {} may only register app.{} and names under it", p.app, p.app),
                    )
                }
                Err(why) => return conn.fail(id, Code::PermissionDenied, why),
            }
        }
        {
            let mut st = self.state();
            match st.services.get(&name) {
                Some(owner) if owner.id == conn.id => {}
                Some(owner) => {
                    let pid = owner.cred.pid;
                    drop(st);
                    return conn.fail(id, Code::AlreadyRegistered, format!("{name:?} is owned by pid {pid}"));
                }
                None => {
                    st.services.insert(name.clone(), Arc::clone(conn));
                }
            }
        }
        log!("{name} registered by pid {}", conn.cred.pid);
        conn.reply(id, Status::ok(), Vec::new());
    }

    fn builtin(&self, conn: &Conn, call: &Call) -> Result<Vec<u8>, Status> {
        match call.method.as_str() {
            "Ping" => {
                let req = proto::PingRequest::decode(call.payload.as_slice()).map_err(Status::bad_request)?;
                Ok(proto::PingReply {
                    message: req.message,
                    broker_version: VERSION.into(),
                    uptime_ms: self.started.elapsed().as_millis() as u64,
                    caller: Some(conn.cred),
                }
                .encode_to_vec())
            }
            "ListServices" => {
                let mut services = vec![proto::ServiceInfo { name: BUS_SERVICE.into(), pid: 0, uid: 0 }];
                services.extend(self.state().services.iter().map(|(name, c)| proto::ServiceInfo {
                    name: name.clone(),
                    pid: c.cred.pid,
                    uid: c.cred.uid,
                }));
                Ok(proto::ListServicesReply { services }.encode_to_vec())
            }
            m => Err(Status::unknown_method(m)),
        }
    }

    /// Forgets a closed connection: its names, the calls it was serving
    /// (their callers get UNAVAILABLE) and the calls it was waiting on.
    fn disconnect(&self, conn: &Arc<Conn>) {
        let (names, orphans) = {
            let mut st = self.state();
            let names: Vec<String> =
                st.services.iter().filter(|(_, c)| c.id == conn.id).map(|(n, _)| n.clone()).collect();
            for n in &names {
                st.services.remove(n);
            }
            let ids: Vec<u64> = st
                .pending
                .iter()
                .filter(|(_, p)| p.target == conn.id || p.caller.id == conn.id)
                .map(|(id, _)| *id)
                .collect();
            let orphans: Vec<Pending> = ids
                .into_iter()
                .filter_map(|id| st.pending.remove(&id))
                .filter(|p| p.caller.id != conn.id)
                .collect();
            (names, orphans)
        };
        for p in orphans {
            p.caller.fail(p.caller_id, Code::Unavailable, format!("{}: the service went away", p.what));
        }
        for n in names {
            log!("{n} unregistered (pid {} disconnected)", conn.cred.pid);
        }
        if self.verbose {
            log!("connection {} closed", conn.id);
        }
        let _ = conn.writer.lock().unwrap_or_else(|p| p.into_inner()).shutdown(Shutdown::Both);
    }

    fn sweep_forever(&self) {
        loop {
            std::thread::sleep(SWEEP_EVERY);
            let now = Instant::now();
            let expired: Vec<Pending> = {
                let mut st = self.state();
                let ids: Vec<u64> =
                    st.pending.iter().filter(|(_, p)| p.deadline <= now).map(|(id, _)| *id).collect();
                ids.into_iter().filter_map(|id| st.pending.remove(&id)).collect()
            };
            for p in expired {
                p.caller.fail(p.caller_id, Code::Timeout, format!("{}: no reply in time", p.what));
            }
        }
    }
}

/// Service names: 1–64 characters of a-z, 0-9, '.', '_' and '-', starting
/// with a letter or digit.
pub fn valid_name(name: &str) -> Result<(), String> {
    let ok_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
    let first_ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if name.is_empty() || name.len() > 64 || !first_ok || !name.chars().all(ok_char) {
        return Err(format!("bad service name {name:?}: use 1-64 of a-z 0-9 . _ - starting with a letter or digit"));
    }
    Ok(())
}

fn is_transient_accept_error(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM | libc::ECONNABORTED))
}

fn peer_cred(stream: &UnixStream) -> io::Result<Caller> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most `len` bytes into `cred`.
    let r = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Caller { uid: cred.uid, gid: cred.gid, pid: cred.pid })
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn names() {
        for ok in ["logd", "netd", "sysinfo", "app.org.example.notes", "a", "x-1_2"] {
            assert!(valid_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Logd", ".x", "-x", "a b", "a/b", &"x".repeat(65)] {
            assert!(valid_name(bad).is_err(), "{bad}");
        }
    }
}
