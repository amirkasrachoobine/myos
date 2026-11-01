use crate::client::send_and_wait;
use crate::frame::{read_frame, write_frame};
use crate::proto::{frame::Kind, Caller, Frame, Register, Reply};
use crate::{Error, Result, Status};
use prost::Message;
use std::io;
use std::os::unix::net::UnixStream;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::time::Duration;

/// A connection that owns one or more service names.
pub struct Service {
    stream: UnixStream,
    names: Vec<String>,
}

/// One incoming call, as seen by a handler.
pub struct Request<'a> {
    /// Which of this connection's names was called.
    pub service: &'a str,
    pub method: &'a str,
    pub payload: &'a [u8],
    /// Who called, from the kernel via the broker. Use it for access checks.
    pub caller: Caller,
}

impl Request<'_> {
    /// Decodes the payload as the method's request type; a BAD_REQUEST
    /// status if it doesn't decode.
    pub fn decode<M: Message + Default>(&self) -> std::result::Result<M, Status> {
        M::decode(self.payload).map_err(|e| Status::bad_request(format!("{}.{}: {e}", self.service, self.method)))
    }
}

/// Serializes a handler's reply message.
pub fn reply<M: Message>(m: &M) -> std::result::Result<Vec<u8>, Status> {
    Ok(m.encode_to_vec())
}

impl Service {
    /// Connects to the broker and claims `names`. Fails if any name is taken
    /// (ALREADY_REGISTERED) or not allowed for this user (PERMISSION_DENIED).
    pub fn register(names: &[&str]) -> Result<Service> {
        Service::register_at(crate::socket_path(), names)
    }

    ///
    /// If the broker isn't listening yet (both start at boot, and init
    /// counts busd as up as soon as it's started), keeps trying for 5 s.
    pub fn register_at(path: impl AsRef<Path>, names: &[&str]) -> Result<Service> {
        let path = path.as_ref();
        let give_up = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match UnixStream::connect(path) {
                Ok(s) => break s,
                Err(e)
                    if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
                        && std::time::Instant::now() < give_up =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(io::Error::new(e.kind(), format!("can't connect to {}: {e}", path.display())).into())
                }
            }
        };
        for (i, name) in names.iter().enumerate() {
            let frame = Frame { id: i as u64 + 1, kind: Some(Kind::Register(Register { service: name.to_string() })) };
            send_and_wait(&mut stream, frame, Duration::from_secs(5))?;
        }
        Ok(Service { stream, names: names.iter().map(|n| n.to_string()).collect() })
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Handles calls one at a time until the connection breaks, which only
    /// happens if the broker goes away. Returns that error; under init the
    /// service then exits and is restarted, re-registering with the new
    /// broker.
    ///
    /// The handler returns the serialized reply (see [`reply`]) or a
    /// [`Status`] for the caller. A panic in the handler becomes INTERNAL.
    pub fn serve<F>(mut self, mut handler: F) -> Result<()>
    where
        F: FnMut(&Request) -> std::result::Result<Vec<u8>, Status>,
    {
        self.stream.set_read_timeout(None)?;
        loop {
            let frame = match read_frame(&mut self.stream)? {
                Some(f) => f,
                None => {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the broker closed the connection").into())
                }
            };
            let Some(Kind::Call(call)) = frame.kind else {
                continue; // nothing else is addressed to a service
            };
            let req = Request {
                service: &call.service,
                method: &call.method,
                payload: &call.payload,
                caller: call.caller.unwrap_or_default(),
            };
            let reply = match catch_unwind(AssertUnwindSafe(|| handler(&req))) {
                Ok(Ok(payload)) => Reply { status: Some(Status::ok()), payload },
                Ok(Err(status)) => Reply { status: Some(status), payload: Vec::new() },
                Err(_) => Reply { status: Some(Status::internal("the service panicked")), payload: Vec::new() },
            };
            write_frame(&mut self.stream, &Frame { id: frame.id, kind: Some(Kind::Reply(reply)) }).map_err(Error::Io)?;
        }
    }
}
