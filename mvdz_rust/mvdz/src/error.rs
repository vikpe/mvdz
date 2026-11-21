#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Not an MVDZ file, or an unsupported version.
    Format(&'static str),
    /// Damaged, or breaks a checksum, structure or limit of FORMAT.md.
    Corrupt(&'static str),
    /// Decoding would exceed the memory limit.
    Limit(&'static str),
    Invalid(&'static str),
    Io(std::io::ErrorKind),
    /// The encoder could not produce a file that decodes to its input.
    Internal(&'static str),
    NotMvd(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(message) => write!(formatter, "format: {message}"),
            Self::Corrupt(message) => write!(formatter, "corrupt: {message}"),
            Self::Limit(message) => write!(formatter, "limit: {message}"),
            Self::Invalid(message) => write!(formatter, "invalid argument: {message}"),
            Self::Io(kind) => write!(formatter, "i/o: {kind}"),
            Self::Internal(message) => write!(formatter, "internal: {message}"),
            Self::NotMvd(message) => write!(formatter, "not an MVD demo: {message}"),
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
pub(crate) fn bad<T>(what: &'static str) -> Result<T> {
    Err(Error::Corrupt(what))
}
