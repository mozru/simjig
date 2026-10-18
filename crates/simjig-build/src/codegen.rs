//! Code generation: the SystemVerilog type IR in, generated sources out.
//!
//! One lowering feeds every emitter (DECISION.md table 2, row 5):
//! - [`lower`] turns an [`svtypes::ir::Design`] and a [`CodegenConfig`] into a
//!   [`Lowered`]. It makes every naming, slot, storage and access decision.
//! - [`emit_rust`]: the typed testbench API (packed structs, enums, unions, package
//!   constants and the `Dut` struct).
//! - `emit_vlt`: the Verilator control file making probed signals public.
//! - `emit_layout_check`: an SV module that fails elaboration if Verilator's layout of a
//!   type differs from the IR.
//! - `emit_wrapper` and `emit_elab_top`: the flat-port top for a DUT with interface
//!   ports.
//! - `emit_shim`: the per-model C++ shim that exports the model's vtable.
//!
//! The module is pure: no file system, no processes. Output is deterministic (a plain
//! indenting writer, IR order everywhere).

mod config;
mod error;
mod lower;
mod lowered;
mod names;
mod rust;
mod writer;

pub use config::{CodegenConfig, IfaceBinding, IfaceTop, Probe, ProbeAccess, ProbeType};
pub use error::Error;
pub use lower::{elab_module, lower, lower_wrapper, wrapper_module};
pub use lowered::{
    Access, Bundle, BundleMember, Const, ConstGroup, ConstValue, DutField, EnumVariant, Item,
    ItemKind, Layout, LayoutCheck, LayoutMember, LayoutValue, Lowered, RustType, Signal,
    SignalTarget, StructField, UnionView, Wrapper, WrapperIface, WrapperPort,
};
pub use rust::emit_rust;
