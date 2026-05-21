use std::fmt;

/// This crate specific `Error` type.
#[derive(Debug)]
pub enum Error {
    /// I/O error.
    Io(std::io::Error),
    /// WebSocket protocol error.
    WebSocket(shiguredo_websocket::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::WebSocket(e) => write!(f, "WebSocket error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::WebSocket(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<shiguredo_websocket::Error> for Error {
    fn from(e: shiguredo_websocket::Error) -> Self {
        Error::WebSocket(e)
    }
}
