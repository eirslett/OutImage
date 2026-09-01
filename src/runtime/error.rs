use std::fmt;
use std::io;

#[derive(Debug)]
pub enum RuntimeError {
    Io(io::Error),
    NotFound(String),
    InvalidArgument(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "runtime I/O error: {error}"),
            Self::NotFound(path) => write!(f, "runtime error: path not found: {path}"),
            Self::InvalidArgument(message) => write!(f, "runtime error: {message}"),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
