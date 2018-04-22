//! WebSocket to TCP proxy server.
//!
//! # References
//!
//! - [RFC 6455] The WebSocket Protocol
//!
//! [RFC 6455]: https://tools.ietf.org/html/rfc6455
#![warn(missing_docs)]
extern crate base64;
extern crate bytecodec;
extern crate byteorder;
extern crate fibers;
extern crate futures;
extern crate httpcodec;
extern crate sha1;
#[macro_use]
extern crate slog;
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
