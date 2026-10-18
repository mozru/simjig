//! Build-time support: runs Verilator, compiles the model, and links it into the crate
//! whose `build.rs` calls it.
//!
//! ```no_run
//! // build.rs
//! simjig_build::Dut::new("counter")
//!     .sources(["rtl/counter.sv"])
//!     .top("counter")
//!     .build()?;
//! # Ok::<(), simjig_build::Error>(())
//! ```
//!
//! Pipeline (see [`Dut::build_in`]): locate and version-check Verilator, run the JSON pass
//! for the type IR, run the [`Emitter`] on it, verilate through the shared content-hash
//! cache, cross-check the header ports against the IR, compile libverilated (once per
//! configuration, cached) and the model with `cc`, and emit `cargo::` directives.
#![forbid(unsafe_code)]

mod archive;
mod cache;
pub mod codegen;
mod compile;
mod depfile;
mod dut;
pub mod emit;
mod env;
mod error;
mod hash;
mod header;
mod iface_diag;
mod manifest;
mod pipeline;
mod runtime;
mod script;
mod syms;
mod verilator;

pub use codegen::{IfaceBinding, Probe, ProbeAccess, ProbeType};
pub use compile::CppRecipe;
pub use dut::{Built, CacheStatus, Dut, DutConfig, Features, GeneratedInput};
pub use emit::{Emitted, Emitter, ProbeSite, SvModule, codegen_emitter, ir_ports, noop_emitter};
pub use env::BuildEnv;
pub use error::Error;
pub use header::{
    HeaderPort, PortDir, PortMismatch, PortProblem, check_ports, parse_header_ports,
    read_header_ports,
};
/// The type IR crate, re-exported so emitters name the same `Design` as the pipeline.
pub use svtypes;
pub use verilator::Verilator;

/// Version of the C ABI between generated Rust and the C++ runtime. It is part of every
/// cache key, so changing the ABI invalidates every cached model and runtime. The C ABI
/// runtime owns its value; bump it whenever the ABI changes.
pub const ABI_VERSION: u32 = 1;
