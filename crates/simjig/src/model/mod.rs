//! Model FFI: generated Verilator models behind the kernel's backend interface.
//!
//! - `abi`: the Rust mirror of `simjig_abi.h` and the raw member accessors;
//! - `raw`: `RawModel`, the owner of one live model handle (the other `unsafe` module);
//! - `verilator`: `VerilatorModel`, the kernel backend (load checks, messages, poisoning);
//! - `meta`: what generated code hands the runtime (`ModelMeta`, `SignalMeta`,
//!   `link_model!`) and the `Binder` that makes typed handles.

#[expect(
    dead_code,
    reason = "the mirror covers the whole ABI; tracing and coverage are not used yet"
)]
#[expect(
    unsafe_code,
    reason = "FFI mirror of simjig_abi.h: raw member pointers and extern vtables"
)]
pub(crate) mod abi;
pub(crate) mod meta;
mod raw;
mod verilator;
