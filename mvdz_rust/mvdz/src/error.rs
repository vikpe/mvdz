#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    Corrupt(&'static str),
    DemoTooLarge,
    Internal(&'static str),
    Io(std::io::ErrorKind),
    MemoryLimit(&'static str),
    NotMvd(&'static str),
    NotMvdz,
    UnsupportedVersion,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(message) => write!(formatter, "corrupt: {message}"),
            Self::DemoTooLarge => write!(formatter, "demo too large: more than 65536 segments"),
            Self::Internal(message) => write!(formatter, "internal: {message}"),
            Self::Io(kind) => write!(formatter, "i/o: {kind}"),
            Self::MemoryLimit(message) => write!(formatter, "memory limit: {message}"),
            Self::NotMvd(message) => write!(formatter, "not an MVD demo: {message}"),
            Self::NotMvdz => write!(formatter, "not an MVDZ file"),
            Self::UnsupportedVersion => write!(formatter, "unsupported MVDZ version"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        match error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Self>())
        {
            Some(own) => *own,
            None => Self::Io(error.kind()),
        }
    }
}

impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Io(kind) => Self::new(kind, error),
            _ => Self::new(std::io::ErrorKind::InvalidData, error),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[inline]
pub(crate) fn corrupt<T>(rule: &'static str) -> Result<T> {
    Err(Error::Corrupt(rule))
}
