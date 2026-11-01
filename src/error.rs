//! The crate's error type: either an I/O failure with what was being done and to which path,
//! or a plain explanation (an invalid job file, a bad setting). Messages are written for the
//! person reading them in a terminal or in a job's `.status.json`.

use std::fmt;
use std::io;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum Error {
    /// `context` says what was being attempted, e.g. "cannot write /tmp/x/run.json".
    Io { context: String, source: io::Error },
    /// A problem described in full by its message.
    Invalid(String),
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Invalid(_) => None,
        }
    }
}

/// An `Err(Error::Invalid)` with the given message.
pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(message.into()))
}

/// Attaches what was being done, and to which path, to an I/O error.
pub(crate) trait IoContext<T> {
    fn context(self, action: &str, path: &Path) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn context(self, action: &str, path: &Path) -> Result<T> {
        self.map_err(|source| Error::Io { context: format!("cannot {action} {}", path.display()), source })
    }
}
