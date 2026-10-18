//! Errors of the lowering and the emitters. Each names the SystemVerilog item and says
//! what to change.

use std::fmt;

/// A design or configuration the code generator cannot handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The IR uses a construct that has no generated form yet.
    Unsupported {
        /// The SystemVerilog item, e.g. `port top.ua` or `type pkg::big_e`.
        item: String,
        /// What is unsupported and what to do instead.
        reason: String,
    },
    /// The configuration does not match the design.
    Config {
        /// The configuration entry, e.g. `probe top.u.x` or `bind_iface(in_bus)`.
        item: String,
        /// What is wrong and how to fix it.
        reason: String,
    },
}

impl Error {
    pub(crate) fn unsupported(item: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unsupported {
            item: item.into(),
            reason: reason.into(),
        }
    }

    pub(crate) fn config(item: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Config {
            item: item.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { item, reason } => {
                write!(f, "codegen: {item}: unsupported: {reason}")
            }
            Self::Config { item, reason } => write!(f, "codegen: {item}: {reason}"),
        }
    }
}

impl std::error::Error for Error {}
