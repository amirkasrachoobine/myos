use crate::proto::Status;
use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Couldn't reach the broker, or the connection broke.
    Io(io::Error),
    /// The call reached the bus but failed: unknown service, the service
    /// returned an error, it timed out, and so on.
    Status(Status),
    /// No reply within the client's timeout. The connection is closed
    /// afterwards, since a late reply could still arrive on it.
    Timeout,
    /// The peer sent something that doesn't follow wire.proto.
    Protocol(String),
}

impl Error {
    /// The bus status code, if the failure came from the bus or a service.
    pub fn code(&self) -> Option<crate::Code> {
        match self {
            Error::Status(s) => Some(s.code()),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "bus connection: {e}"),
            Error::Status(s) => write!(f, "{s}"),
            Error::Timeout => write!(f, "timed out waiting for a reply"),
            Error::Protocol(m) => write!(f, "bus protocol error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<Status> for Error {
    fn from(s: Status) -> Self {
        Error::Status(s)
    }
}
