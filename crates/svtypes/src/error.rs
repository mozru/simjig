//! The crate's error type.
//!
//! Every error names what to look at: a JSON path plus producer version for front-end
//! errors, the command line plus a stderr excerpt for runner errors.

use std::fmt;
use std::io;

/// Anything that can go wrong while producing a [`Design`](crate::ir::Design).
#[derive(Debug)]
pub enum Error {
    /// The producer's output contains something this front end does not understand, or
    /// violates an invariant (a width mismatch, a dangling reference).
    Json {
        /// JSON pointer of the offending node or field, e.g. `/modulesp/0/stmtsp/3`.
        path: String,
        /// Source location of the node, `file:line:col`, when known.
        source_loc: Option<String>,
        /// Producer version whose dialect was being decoded.
        version: String,
        /// What is wrong.
        message: String,
    },
    /// No front end decodes this producer version.
    UnsupportedVersion {
        /// The version string as reported by the tool.
        version: String,
    },
    /// The producer ran and failed.
    Tool {
        /// The full command line.
        command: String,
        /// Exit status, if the process exited normally.
        status: Option<i32>,
        /// The tail of the tool's standard error.
        stderr: String,
    },
    /// An I/O operation failed.
    Io {
        /// What was being done.
        context: String,
        /// The underlying error.
        source: io::Error,
    },
}

/// Convenience alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Json {
                path,
                source_loc,
                version,
                message,
            } => {
                write!(f, "Verilator {version} JSON at {path}")?;
                if let Some(loc) = source_loc {
                    write!(f, " ({loc})")?;
                }
                write!(f, ": {message}")
            }
            Error::UnsupportedVersion { version } => write!(
                f,
                "no svtypes front end for `{version}`; supported: Verilator 5.050 to 5.059"
            ),
            Error::Tool {
                command,
                status,
                stderr,
            } => {
                let status =
                    status.map_or_else(|| "a signal".to_owned(), |s| format!("status {s}"));
                write!(f, "`{command}` failed with {status}\n{stderr}")
            }
            Error::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
