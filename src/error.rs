use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Format(&'static str),
    Info(String),
    Io(std::io::Error),
    Message(String),
    Unsupported(&'static str),
    Usage(&'static str),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Info(_) => 0,
            Error::Usage(_) => 2,
            Error::Format(_) => 3,
            Error::Unsupported(_) => 4,
            Error::Io(_) => 1,
            Error::Message(_) => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Format(message) => write!(formatter, "format error: {message}"),
            Error::Info(message) => write!(formatter, "{message}"),
            Error::Io(error) => write!(formatter, "{error}"),
            Error::Message(message) => write!(formatter, "{message}"),
            Error::Unsupported(message) => write!(formatter, "unsupported: {message}"),
            Error::Usage(message) => write!(formatter, "usage error: {message}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Io(error)
    }
}
