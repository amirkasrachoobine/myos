use crate::frame::{read_frame, write_frame};
use crate::proto::{self, frame::Kind, Call, Frame};
use crate::{Error, Result, BUS_SERVICE};
use prost::Message;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a call waits by default. The broker is told the same deadline,
/// so it normally answers TIMEOUT first; the client waits a little longer
/// before giving up on the connection itself.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const GRACE: Duration = Duration::from_secs(1);

/// A connection to the bus for making calls. One call at a time; open
/// several clients (they're cheap) for concurrent calls.
pub struct Client {
    stream: Option<UnixStream>,
    next_id: u64,
    timeout: Duration,
}

impl Client {
    /// Connects to the broker at [`crate::socket_path`].
    pub fn connect() -> Result<Client> {
        Client::connect_to(crate::socket_path())
    }

    pub fn connect_to(path: impl AsRef<Path>) -> Result<Client> {
        let path = path.as_ref();
        let stream = UnixStream::connect(path)
            .map_err(|e| io::Error::new(e.kind(), format!("can't connect to {}: {e}", path.display())))?;
        Ok(Client { stream: Some(stream), next_id: 0, timeout: DEFAULT_TIMEOUT })
    }

    /// Changes the per-call timeout (default 5 s).
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout.max(Duration::from_millis(1));
    }

    /// Calls `service`.`method` with a typed request and decodes the reply.
    pub fn call<Req: Message, Rep: Message + Default>(&mut self, service: &str, method: &str, req: &Req) -> Result<Rep> {
        let bytes = self.call_raw(service, method, req.encode_to_vec())?;
        Rep::decode(bytes.as_slice()).map_err(|e| Error::Protocol(format!("{service}.{method} reply didn't decode: {e}")))
    }

    /// Calls `service`.`method` with an already-serialized payload.
    pub fn call_raw(&mut self, service: &str, method: &str, payload: Vec<u8>) -> Result<Vec<u8>> {
        let timeout = self.timeout;
        let Some(stream) = self.stream.as_mut() else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "connection closed after an earlier error").into());
        };
        self.next_id += 1;
        let id = self.next_id;
        let call = Call {
            service: service.into(),
            method: method.into(),
            payload,
            caller: None,
            timeout_ms: timeout.as_millis().min(u32::MAX as u128) as u32,
        };
        let result = exchange(stream, id, call, timeout + GRACE);
        if matches!(result, Err(Error::Io(_) | Error::Timeout | Error::Protocol(_))) {
            // The stream may be mid-frame or carry a late reply: don't reuse it.
            self.stream = None;
        }
        result
    }

    /// Round trip through the broker.
    pub fn ping(&mut self, message: &str) -> Result<proto::PingReply> {
        self.call(BUS_SERVICE, "Ping", &proto::PingRequest { message: message.into() })
    }

    /// Services currently registered on the bus.
    pub fn list_services(&mut self) -> Result<Vec<proto::ServiceInfo>> {
        let r: proto::ListServicesReply = self.call(BUS_SERVICE, "ListServices", &proto::ListServicesRequest {})?;
        Ok(r.services)
    }
}

/// Sends one frame with `id` and waits for the reply with the same id.
pub(crate) fn exchange(stream: &mut UnixStream, id: u64, call: Call, wait: Duration) -> Result<Vec<u8>> {
    send_and_wait(stream, Frame { id, kind: Some(Kind::Call(call)) }, wait)
}

pub(crate) fn send_and_wait(stream: &mut UnixStream, frame: Frame, wait: Duration) -> Result<Vec<u8>> {
    let id = frame.id;
    write_frame(stream, &frame)?;
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout);
        }
        stream.set_read_timeout(Some(left))?;
        match read_frame(stream) {
            Ok(Some(Frame { id: got, kind })) if got == id => {
                return match kind {
                    Some(Kind::Reply(r)) => {
                        let status = r.status.unwrap_or_default();
                        if status.is_ok() {
                            Ok(r.payload)
                        } else {
                            Err(Error::Status(status))
                        }
                    }
                    _ => Err(Error::Protocol(format!("expected a reply to frame {id}"))),
                };
            }
            Ok(Some(_)) => continue, // not ours; can't normally happen
            Ok(None) => {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the broker closed the connection").into())
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                return Err(Error::Timeout)
            }
            Err(e) => return Err(e.into()),
        }
    }
}
