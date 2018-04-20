extern crate bytecodec;
extern crate byteorder;
extern crate fibers;
extern crate futures;
extern crate httpcodec;
#[macro_use]
extern crate slog;
#[macro_use]
extern crate trackable;

pub use error::{Error, ErrorKind};

pub mod proxy;

mod error;

/// This crate specific `Result` type.
pub type Result<T> = std::result::Result<T, Error>;
