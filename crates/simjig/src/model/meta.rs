//! What generated code hands the runtime: the model's metadata ([`ModelMeta`],
//! [`SignalMeta`]), the link to its C vtable ([`link_model!`](crate::__link_model)), and
//! the [`Binder`] that turns slots into typed handles.
//!
//! The generated Rust for one model looks like this (`tests/model_tests/duts.rs` holds
//! hand-written instances):
//!
//! ```text
//! ::simjig::__private::link_model!(fn vtable = "simjig_vtable_Vcounter");
//!
//! pub static MODEL: ::simjig::__private::ModelMeta = ::simjig::__private::ModelMeta {
//!     vtable,
//!     prefix: "Vcounter",
//!     ir_hash: 0x0123_4567_89ab_cdef,
//!     precision_exponent: -12,
//!     signals: Dut::SIGNALS,
//! };
//!
//! pub struct Dut {
//!     pub clk: ::simjig::__private::In<::simjig::__private::bits::Bit>,
//!     pub count: ::simjig::__private::Out<::simjig::__private::bits::Bits<8>>,
//! }
//!
//! impl Dut {
//!     pub const SIGNALS: &'static [::simjig::__private::SignalMeta] = &[
//!         ::simjig::__private::SignalMeta::new("counter.clk", 1, ::simjig::__private::SignalAccess::In),
//!         ::simjig::__private::SignalMeta::new("counter.count", 8, ::simjig::__private::SignalAccess::Out),
//!     ];
//!
//!     pub fn sim() -> ::core::result::Result<(::simjig::Sim, Self), ::simjig::LoadError> {
//!         let (sim, b) = ::simjig::__private::load(&MODEL)?;
//!         Ok((sim, Self { clk: b.bind(0), count: b.bind(1) }))
//!     }
//! }
//! ```
//!
//! Invariant: slot `i` of the C++ signal table, of [`ModelMeta::signals`] and of the kernel are
//! the same signal. [`load`] checks the first two against each other; the kernel numbers its
//! slots from the C++ table.

use std::fmt;

use super::abi::{AbiError, ModelVtable};
use super::verilator::VerilatorModel;
use crate::kernel::signal::RawSig;
use crate::kernel::sim::Sim;
use crate::signal::Bind;

/// A model's C vtable, as returned by its `simjig_vtable_<prefix>` symbol; opaque to Rust code
/// outside simjig.
#[repr(transparent)]
pub struct Vtable(ModelVtable);

impl Vtable {
    pub(crate) fn as_abi(&self) -> &ModelVtable {
        &self.0
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
pub enum SignalAccess {
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

/// One signal of a model, as the generator saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalMeta {
    /// Hierarchical name (`counter.clk`); equals the C++ table's name.
    pub name: &'static str,
    /// Width in bits.
    pub width: u32,
    /// Access kind.
    pub access: SignalAccess,
}

impl SignalMeta {
    /// A signal entry; `const` so generated tables are `static` data.
    #[must_use]
    pub const fn new(name: &'static str, width: u32, access: SignalAccess) -> Self {
        Self {
            name,
            width,
            access,
        }
    }
}

/// Everything the runtime needs to load one generated model.
#[derive(Clone, Copy)]
pub struct ModelMeta {
    /// The model's vtable symbol, declared with [`link_model!`](crate::__link_model).
    pub vtable: fn() -> &'static Vtable,
    /// Verilator `--prefix`; must equal the vtable's.
    pub prefix: &'static str,
    /// The IR hash the generated Rust was made from; must equal the vtable's.
    pub ir_hash: u64,
    /// The design's time precision as a power of ten of seconds (`-12` is 1 ps).
    pub precision_exponent: i8,
    /// Every signal, in slot order.
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

/// Loads the model `meta` describes into a new [`Sim`], and returns the [`Binder`] that makes
/// its typed handles.
///
/// # Errors
/// See [`LoadError`]: the model and the generated Rust disagree, or the model failed while
/// being constructed.
pub fn load(meta: &'static ModelMeta) -> Result<(Sim, Binder), LoadError> {
    let model = VerilatorModel::load(meta)?;
    let sim = Sim::from_backend(Box::new(model));
    // `Sim` keeps its id private; any signal's handle carries it. A model without signals
    // has nothing to bind, so its id is never needed.
    let sim_gen = meta
        .signals
        .first()
        .and_then(|s| sim.signal(s.name))
        .map_or(0, RawSig::sim_gen);
    let binder = Binder {
        sim_gen,
        signals: meta.signals,
    };
    Ok((sim, binder))
}

/// Makes the typed handles of one loaded model; only [`load`] creates it.
#[derive(Debug)]
pub struct Binder {
    sim_gen: u32,
    signals: &'static [SignalMeta],
}

impl Binder {
    /// The handle of `slot`, typed by the field it is assigned to.
    ///
    /// # Panics
    /// If `slot` is out of range, or its width or access kind does not match `H` (a generator
    /// bug: the metadata and the field types come from the same lowering).
    #[track_caller]
    #[must_use]
    pub fn bind<H: Bind>(&self, slot: u32) -> H {
        let Some(meta) = self.signals.get(slot as usize) else {
            panic!(
                "generated code binds slot {slot}, but the model has {} signals",
                self.signals.len()
            );
        };
        assert!(
            meta.access == H::ACCESS && meta.width == H::WIDTH,
            "generated code binds `{}` ({} bits, {:?}) as a {:?} handle of {} bits",
            meta.name,
            meta.width,
            meta.access,
            H::ACCESS,
            H::WIDTH
        );
        H::from_raw(RawSig::new(self.sim_gen, slot))
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
