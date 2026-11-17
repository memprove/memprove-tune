//! The one error type of the crate.

use std::{fmt, io, path::PathBuf};

/// Why an eval operation could not be done. Every variant's text is a
/// sentence a person or an agent can act on.
#[derive(Debug)]
pub enum EvalError {
    /// A file could not be read or written.
    Io {
        /// The file or directory.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// A task or run file is not valid.
    Invalid {
        /// The file, when the input came from one.
        path: Option<PathBuf>,
        /// What is wrong, naming the member.
        detail: String,
    },
    /// A `git` command failed.
    Git {
        /// The arguments after `git`.
        args: Vec<String>,
        /// Its standard error, trimmed.
        stderr: String,
    },
    /// A command could not be started.
    Spawn {
        /// The program.
        program: String,
        /// What the operating system said.
        source: io::Error,
    },
    /// A usage error on the command line.
    Usage(String),
    /// A part of the crate that a later change provides.
    Unimplemented(&'static str),
}

impl EvalError {
    /// An [`EvalError::Invalid`] for `path`.
    #[must_use]
    pub fn invalid(path: Option<&std::path::Path>, detail: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.map(std::path::Path::to_path_buf),
            detail: detail.into(),
        }
    }

    /// An [`EvalError::Io`] for `path`.
    #[must_use]
    pub fn io(path: &std::path::Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Invalid {
                path: Some(path),
                detail,
            } => write!(f, "{}: {detail}", path.display()),
            Self::Invalid { path: None, detail } | Self::Usage(detail) => f.write_str(detail),
            Self::Git { args, stderr } => {
                write!(f, "git {} failed: {stderr}", args.join(" "))
            }
            Self::Spawn { program, source } => write!(f, "cannot start {program}: {source}"),
            Self::Unimplemented(part) => write!(f, "{part} is not implemented yet"),
        }
    }
}

impl std::error::Error for EvalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}
