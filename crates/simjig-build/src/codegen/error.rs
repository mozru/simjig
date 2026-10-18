//! Errors of the lowering and the emitters. Each names the SystemVerilog item and says
//! what to change.

use std::fmt;

/// A design or configuration the code generator cannot handle.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The IR uses a construct that has no generated form yet.
    Unsupported {
        /// The SystemVerilog item, e.g. `port top.ua` or `type pkg::big_e`.
        item: String,
        /// What is unsupported and what to do instead.
        reason: String,
    },
    /// A probe does not match the design (its path, module, type or access).
    Probe {
        /// The probe's path, as configured.
        path: String,
        /// What is wrong and how to fix it.
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

/// The reason for an IR construct this `simjig-build` does not know: the IR's enums are
/// `#[non_exhaustive]`, and a newer `svtypes` may produce a kind added after this release.
pub(crate) const UNKNOWN_IR_KIND: &str = "a kind of IR construct this simjig-build does not know (from a newer svtypes); update \
     simjig-build";

impl Error {
    pub(crate) fn unsupported(item: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unsupported {
            item: item.into(),
            reason: reason.into(),
        }
    }

    pub(crate) fn probe(path: &str, reason: impl Into<String>) -> Self {
        Self::Probe {
            path: path.to_owned(),
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
            Self::Probe { path, reason } => write!(f, "codegen: probe {path}: {reason}"),
            Self::Config { item, reason } => write!(f, "codegen: {item}: {reason}"),
        }
    }
}

impl std::error::Error for Error {}

/// The message: `Debug` is what `fn main() -> Result<..>` and `unwrap` print.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        crate::error::debug_chain(self, f)
    }
}
