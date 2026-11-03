//! Client and service library for the myos IPC bus.
//!
//! ```no_run
//! // Call a method:
//! let mut bus = myos_bus::Client::connect()?;
//! let pong = bus.ping("hello")?;
//! println!("{} via {}", pong.message, pong.broker_version);
//! # Ok::<(), myos_bus::Error>(())
//! ```
//!
//! ```no_run
//! // Provide a service:
//! use myos_bus::{proto, Service, Status};
//! Service::register(&["sysinfo"])?.serve(|req| match req.method {
//!     "Ping" => {
//!         let r: proto::PingRequest = req.decode()?;
//!         myos_bus::reply(&proto::PingReply { message: r.message, ..Default::default() })
//!     }
//!     m => Err(Status::unknown_method(m)),
//! })?;
//! # Ok::<(), myos_bus::Error>(())
//! ```
//!
//! The wire format is described in proto/myos/bus/v1/wire.proto.

mod client;
mod error;
pub mod frame;
mod service;

/// Generated from proto/myos/bus/v1/*.proto.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/myos.bus.v1.rs"));
}

pub use client::Client;
pub use error::{Error, Result};
pub use proto::status::Code;
pub use proto::Status;
pub use service::{reply, Request, Service};

use std::path::PathBuf;

/// Where the broker listens unless $MYOS_BUS_SOCKET says otherwise.
pub const DEFAULT_SOCKET: &str = "/run/myos/bus.sock";

/// The broker's own service name.
pub const BUS_SERVICE: &str = "bus";

/// Phase 6: every Java app runs under its own uid in this range (jrun
/// allocates them). busd limits what these uids may do on the bus to what
/// the app's policy file allows; see busd's policy.rs.
pub const APP_UID_MIN: u32 = 20000;
pub const APP_UID_MAX: u32 = 29999;

/// Where busd finds app policies: one file per app uid, named by the uid.
pub const DEFAULT_POLICY_DIR: &str = "/etc/myos/bus/policy.d";

/// The socket path clients should use: $MYOS_BUS_SOCKET, or the default.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("MYOS_BUS_SOCKET") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(DEFAULT_SOCKET),
    }
}

impl Status {
    pub fn new(code: Code, message: impl Into<String>) -> Status {
        Status { code: code as i32, message: message.into() }
    }
    pub fn ok() -> Status {
        Status::new(Code::Ok, "")
    }
    pub fn is_ok(&self) -> bool {
        self.code() == Code::Ok
    }
    pub fn unknown_method(method: &str) -> Status {
        Status::new(Code::UnknownMethod, format!("no method {method:?}"))
    }
    pub fn bad_request(why: impl std::fmt::Display) -> Status {
        Status::new(Code::BadRequest, why.to_string())
    }
    pub fn internal(why: impl std::fmt::Display) -> Status {
        Status::new(Code::Internal, why.to_string())
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.code().as_str_name())
        } else {
            write!(f, "{}: {}", self.code().as_str_name(), self.message)
        }
    }
}
