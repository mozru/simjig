//! Code generation: the SystemVerilog type IR in, generated sources out.
//!
//! One lowering feeds every emitter (DECISION.md table 2, row 5):
//! - [`lower`] turns an [`svtypes::ir::Design`] and a [`CodegenConfig`] into a
//!   [`Lowered`]. It makes every naming, slot, storage and access decision.
//! - [`emit_rust`]: the typed testbench API (packed structs, enums, unions, package
//!   constants and the `Dut` struct).
//! - [`emit_vlt`]: the Verilator control file making probed signals public.
//! - [`emit_layout_check`]: an SV module that fails elaboration if Verilator's layout of a
//!   type differs from the IR.
//! - [`lower_sweep`] and [`emit_sweep`]: several configurations of one DUT module as one
//!   sweep module (shared types, the `Ports` trait); [`emit_sweep_member`] is a member's
//!   [`emit_rust`], importing the shared types from the sweep module.
//! - [`emit_wrapper`] and [`emit_elab_top`]: the flat-port top for a DUT with interface
//!   ports.
//! - [`emit_shim`]: the per-model C++ shim that exports the model's vtable.
//! - [`emit_dpi`]: the Rust side of the design's DPI-C imports (a trait, `Dut::set_dpi`
//!   and `extern "C"` trampolines), appended to the Rust bindings.
//!
//! The module is pure: no file system, no processes. Output is deterministic (a plain
//! indenting writer, IR order everywhere).

mod config;
mod dpi;
mod error;
mod layout;
mod lower;
mod lowered;
mod names;
mod rust;
mod shim;
mod vlt;
mod wrapper;
mod writer;

pub(crate) use names::did_you_mean;

pub use config::{CodegenConfig, IfaceBinding, IfaceTop, Probe, ProbeAccess, ProbeType};
pub use dpi::{DpiHeaderMismatch, check_dpi_header, dpi_prototypes, emit_dpi};
pub use error::Error;
pub use layout::{LAYOUT_CHECK_FLAGS, emit_layout_check};
pub use lower::{
    DUT_INSTANCE, PORTS_TRAIT, elab_module, lower, lower_sweep, lower_wrapper, wrapper_module,
};
pub use lowered::{
    Access, AssocBound, AssocType, Bundle, BundleMember, Const, ConstGroup, ConstValue, DutField,
    EnumVariant, Item, ItemKind, Layout, LayoutCheck, LayoutMember, LayoutTypedef, LayoutValue,
    Lowered, LoweredSweep, MethodKind, PortsTrait, RustType, Signal, SignalTarget, StructField,
    SweepMember, TraitConst, TraitMethod, TraitType, UnionView, Wrapper, WrapperIface, WrapperPort,
};
pub use rust::{RustOptions, emit_rust, emit_sweep, emit_sweep_member};
pub use shim::{ShimOptions, emit_shim};
pub use vlt::emit_vlt;
pub use wrapper::{emit_elab_top, emit_wrapper};
