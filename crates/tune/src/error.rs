//! The one error type of the crate.

use std::{fmt, io, path::PathBuf};

use tune_engine::EvalError;

/// Why a command could not be done; every text is a sentence a person or
/// an agent can act on.
#[derive(Debug)]
pub(crate) enum Error {
    /// The command line is wrong.
    Usage(String),
    /// Something the command read is not what it needs.
    Invalid(String),
    /// A file could not be read or written.
    Io {
        /// The file or directory.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// The trial engine failed.
    Engine(EvalError),
}

impl Error {
    /// An [`Error::Io`] for `path`.
    pub(crate) fn io(path: &std::path::Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(detail) | Self::Invalid(detail) => f.write_str(detail),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Engine(error) => error.fmt(f),
        }
    }
}

impl From<EvalError> for Error {
    fn from(error: EvalError) -> Self {
        match error {
            EvalError::Usage(detail) => Self::Usage(detail),
            other => Self::Engine(other),
        }
    }
}
