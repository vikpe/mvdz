//! The library error type.

/// Library error. New variants may be added in minor releases, so a
/// `match` on it needs a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Not an MVDZ file, or an unsupported version.
    Format(&'static str),
    /// The file is damaged or breaks a rule of the format (any checksum,
    /// structure or limit of FORMAT.md).
    Corrupt(&'static str),
    /// Decoding would exceed the caller's memory or work budget.
    Limit(&'static str),
    /// A bad argument (seek past the end, ...).
    Invalid(&'static str),
    /// An I/O error from the underlying reader or writer.
    Io(std::io::ErrorKind),
    /// The encoder could not produce a file that decodes to its input.
    Internal(&'static str),
    /// The input to compress is not an MVD demo.
    NotMvd(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(s) => write!(f, "format: {s}"),
            Self::Corrupt(s) => write!(f, "corrupt: {s}"),
            Self::Limit(s) => write!(f, "limit: {s}"),
            Self::Invalid(s) => write!(f, "invalid argument: {s}"),
            Self::Io(k) => write!(f, "i/o: {k}"),
            Self::Internal(s) => write!(f, "internal: {s}"),
            Self::NotMvd(s) => write!(f, "not an MVD demo: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        match e.get_ref().and_then(|x| x.downcast_ref::<Self>()) {
            Some(d) => *d,
            None => Self::Io(e.kind()),
        }
    }
}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(k) => Self::new(k, e),
            _ => Self::new(std::io::ErrorKind::InvalidData, e),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Shorthand for a corrupt-file error.
#[inline]
pub(crate) fn bad<T>(what: &'static str) -> Result<T> {
    Err(Error::Corrupt(what))
}
