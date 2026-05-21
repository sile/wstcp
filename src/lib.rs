//! WebSocket to TCP proxy server.
//!
//! # References
//!
//! - [RFC 6455] The WebSocket Protocol
//!
//! [RFC 6455]: https://tools.ietf.org/html/rfc6455
#![warn(missing_docs)]

pub use error::Error;
pub use server::ProxyServer;

mod channel;
mod error;
mod server;
