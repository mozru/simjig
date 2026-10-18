//! What generated code hands the runtime about a model: its signal table
//! ([`SignalMeta`], built by simjig-build's codegen as `Dut::SIGNALS`), its vtable symbol
//! ([`link_model!`](crate::__link_model)) and the rest of [`ModelMeta`]; and the
//! [`LoadError`] when the model and the generated Rust disagree.
//!
//! Invariant: slot `i` of the C++ signal table, of [`ModelMeta::signals`] and of the kernel
//! are the same signal. Loading checks the first two against each other; the kernel numbers
//! its slots from the C++ table.

use std::fmt;

use super::abi::{AbiError, ModelVtable};

/// A model's C vtable, as returned by its `simjig_vtable_<prefix>` symbol; opaque to Rust code
/// outside simjig.
#[repr(transparent)]
pub struct Vtable(ModelVtable);

impl Vtable {
    pub(crate) fn as_abi(&self) -> &ModelVtable {
        &self.0
    }

    /// A vtable built in Rust (the fake model of the backend's unit tests), leaked.
    #[cfg(test)]
    pub(crate) fn leak(vt: ModelVtable) -> &'static Self {
        Box::leak(Box::new(Self(vt)))
    }
}

impl fmt::Debug for Vtable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vtable")
            .field("prefix", &self.0.prefix())
            .field("abi_version", &self.0.abi_version)
            .field("ir_hash", &format_args!("{:#018x}", self.0.ir_hash))
            .finish_non_exhaustive()
    }
}

/// How the testbench may access a signal; decides which handle type binds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    /// A top-level input: [`In`](crate::In).
    In,
    /// A top-level output: [`Out`](crate::Out).
    Out,
    /// A top-level inout: [`InOut`](crate::InOut).
    InOut,
    /// A `public_flat_rd` internal signal: [`PublicRd`](crate::PublicRd).
    PublicRd,
    /// A `public_flat_rw` internal signal: [`PublicRw`](crate::PublicRw).
    PublicRw,
}

/// The C++ type Verilator stores a signal in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Storage {
    /// `CData`, 1 to 8 bits.
    CData,
    /// `SData`, 9 to 16 bits.
    SData,
    /// `IData`, 17 to 32 bits.
    IData,
    /// `QData`, 33 to 64 bits.
    QData,
    /// `VlWide<words>`, more than 64 bits.
    VlWide {
        /// 32-bit words.
        words: u32,
    },
    /// `VlUnpacked<elem, len>`, an unpacked array; not supported by the C ABI yet.
    VlUnpacked {
        /// The element's storage.
        elem: &'static Storage,
        /// Number of elements.
        len: u32,
    },
}

impl Storage {
    /// Verilator's storage for a packed value of `width` bits.
    pub(crate) const fn packed(width: u32) -> Self {
        match width {
            0..=8 => Self::CData,
            9..=16 => Self::SData,
            17..=32 => Self::IData,
            33..=64 => Self::QData,
            _ => Self::VlWide {
                words: width.div_ceil(32),
            },
        }
    }
}

/// One signal of a model, as the generator saw it (one entry of `Dut::SIGNALS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalMeta {
    /// Hierarchical name (`counter.clk`); equals the C++ table's name.
    pub name: &'static str,
    /// Slot number: the index in `Dut::SIGNALS` and in the C++ table.
    pub slot: u32,
    /// Packed width in bits (of one element, for an unpacked array).
    pub width: u32,
    /// Verilator storage.
    pub storage: Storage,
    /// Access kind.
    pub access: Access,
}

/// Everything the runtime needs to load one generated model; built by
/// [`dut_model!`](crate::__dut_model).
#[derive(Clone, Copy)]
pub struct ModelMeta {
    /// The model's vtable symbol, declared with [`link_model!`](crate::__link_model).
    pub vtable: fn() -> &'static Vtable,
    /// Verilator `--prefix`; must equal the vtable's.
    pub prefix: &'static str,
    /// The IR hash the generated Rust was made from (`Dut::IR_HASH`); must equal the
    /// vtable's.
    pub ir_hash: u64,
    /// The design's time precision as a power of ten of seconds (`-12` is 1 ps).
    pub precision_exponent: i8,
    /// Every signal, in slot order (`Dut::SIGNALS`).
    pub signals: &'static [SignalMeta],
}

impl fmt::Debug for ModelMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelMeta")
            .field("prefix", &self.prefix)
            .field("ir_hash", &format_args!("{:#018x}", self.ir_hash))
            .field("precision_exponent", &self.precision_exponent)
            .field("signals", &self.signals.len())
            .finish_non_exhaustive()
    }
}

/// Why a generated model could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The model was compiled against another version of the simjig C ABI.
    AbiVersion {
        /// The model's prefix.
        prefix: &'static str,
        /// The model's `SIMJIG_ABI_VERSION`.
        model: u32,
        /// This runtime's.
        runtime: u32,
    },
    /// The model's vtable is shorter than this runtime expects.
    VtableSize {
        /// The model's prefix.
        prefix: &'static str,
        /// The model's `sizeof(simjig_model_vtable)`.
        model: u32,
        /// This runtime's.
        runtime: usize,
    },
    /// The model and the generated Rust come from different IRs.
    IrHash {
        /// The model's prefix.
        prefix: &'static str,
        /// The model's IR hash.
        model: u64,
        /// The generated Rust's.
        expected: u64,
    },
    /// The vtable symbol belongs to another model.
    Prefix {
        /// The prefix the generated Rust expects.
        expected: &'static str,
        /// The vtable's.
        found: String,
    },
    /// The generated time precision is not a power of ten simjig supports.
    Precision {
        /// The model's prefix.
        prefix: &'static str,
        /// The exponent.
        exponent: i8,
    },
    /// The model's signal table disagrees with the generated Rust or the ABI.
    SignalTable {
        /// The model's prefix.
        prefix: &'static str,
        /// The first slot that disagrees.
        slot: usize,
        /// How it disagrees.
        problem: String,
    },
    /// Constructing the model failed.
    Create {
        /// The model's prefix.
        prefix: &'static str,
        /// What the model reported.
        reason: String,
    },
}

impl LoadError {
    pub(crate) fn from_abi(prefix: &'static str, e: &AbiError) -> Self {
        match *e {
            AbiError::Version { model, runtime } => Self::AbiVersion {
                prefix,
                model,
                runtime,
            },
            AbiError::Size { model, runtime } => Self::VtableSize {
                prefix,
                model,
                runtime,
            },
            AbiError::IrHash { model, expected } => Self::IrHash {
                prefix,
                model,
                expected,
            },
        }
    }
}

const REBUILD: &str = "the model and the generated Rust come from different builds; rebuild \
                       both (cargo clean -p for the crate that builds the model)";

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AbiVersion {
                prefix,
                model,
                runtime,
            } => write!(
                f,
                "model `{prefix}` uses simjig C ABI version {model}, this runtime version \
                 {runtime}; {REBUILD}"
            ),
            Self::VtableSize {
                prefix,
                model,
                runtime,
            } => write!(
                f,
                "model `{prefix}` has a {model}-byte vtable, this runtime needs {runtime}; \
                 {REBUILD}"
            ),
            Self::IrHash {
                prefix,
                model,
                expected,
            } => write!(
                f,
                "model `{prefix}` was built from IR {model:#018x}, the generated Rust from \
                 {expected:#018x}; {REBUILD}"
            ),
            Self::Prefix { expected, found } => write!(
                f,
                "the vtable linked for model `{expected}` belongs to model `{found}`; {REBUILD}"
            ),
            Self::Precision { prefix, exponent } => write!(
                f,
                "model `{prefix}` has time precision 1e{exponent} s, which simjig does not \
                 support (1 s down to 1 fs)"
            ),
            Self::SignalTable {
                prefix,
                slot,
                problem,
            } => write!(
                f,
                "model `{prefix}`, signal slot {slot}: {problem}; {REBUILD}"
            ),
            Self::Create { prefix, reason } => {
                write!(f, "model `{prefix}` failed while being created: {reason}")
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Declares `fn $name() -> &'static Vtable` returning the model's C vtable, from the symbol
/// `simjig_vtable_<prefix>` that the generated C++ shim exports. For generated code only.
///
/// The `unsafe extern` block lives in this macro, so a user crate with
/// `#![forbid(unsafe_code)]` can include generated code (the lint does not fire on code
/// expanded from another crate's macro).
#[doc(hidden)]
#[macro_export]
macro_rules! __link_model {
    ($vis:vis fn $name:ident = $symbol:literal) => {
        $vis fn $name() -> &'static $crate::__private::Vtable {
            // SAFETY: `$symbol` is defined by `SIMJIG_EXPORT_VTABLE` in the model's generated
            // C++ shim. It takes no arguments, cannot fail or unwind, and returns the address
            // of a `static constexpr simjig_model_vtable`: non-null, aligned, immutable and
            // valid for the whole program, which is what `&'static Vtable` promises.
            unsafe extern "C" {
                #[link_name = $symbol]
                safe fn vtable() -> &'static $crate::__private::Vtable;
            }
            vtable()
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_errors_say_what_to_do() {
        let e = LoadError::IrHash {
            prefix: "Vcounter",
            model: 1,
            expected: 2,
        };
        let text = e.to_string();
        assert!(
            text.contains("`Vcounter`") && text.contains("rebuild"),
            "{text}"
        );
    }
}
