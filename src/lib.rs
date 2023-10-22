//! WebSocket to TCP proxy server.
//!
//! # References
//!
//! - [RFC 6455] The WebSocket Protocol
//!
//! [RFC 6455]: https://tools.ietf.org/html/rfc6455
#![warn(missing_docs)]
#[macro_use]
extern crate bytecodec;
#[macro_use]
extern crate trackable;

pub use error::{Error, ErrorKind};
pub use server::ProxyServer;

mod channel;
mod error;
mod frame;
mod opcode;
mod server;
mod util;

/// This crate specific `Result` type.
pub type Result<T> = std::result::Result<T, Error>;
