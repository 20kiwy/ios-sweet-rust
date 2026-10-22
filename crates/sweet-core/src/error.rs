use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Static message (parity with the Python tools' asserts / exits).
    Msg(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Msg(msg.into()))
}

#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => { return Err($crate::Error::Msg(format!($($arg)*))) };
}
