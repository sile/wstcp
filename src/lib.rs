extern crate byteorder;
extern crate fibers;
extern crate futures;
#[macro_use]
extern crate trackable;

pub use error::{Error, ErrorKind};

mod error;

/// This crate specific `Result` type.
pub type Result<T> = std::result::Result<T, Error>;
