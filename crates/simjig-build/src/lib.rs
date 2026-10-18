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

mod cache;
mod compile;
mod depfile;
mod dut;
pub mod emit;
mod env;
mod error;
mod hash;
mod header;
mod runtime;
mod verilator;

pub use compile::CppRecipe;
pub use dut::{Access, Built, CacheStatus, Dut, DutConfig, GeneratedInput, IfaceBinding, Probe};
pub use emit::{Emitted, Emitter, Wrapper, ir_ports, noop_emitter};
pub use env::BuildEnv;
pub use error::Error;
pub use header::{
    HeaderPort, PortDir, PortMismatch, PortProblem, Storage, check_ports, parse_header_ports,
    read_header_ports,
};
/// The type IR crate, re-exported so emitters name the same `Design` as the pipeline.
pub use svtypes;
pub use verilator::{SUPPORTED_VERILATOR, Verilator, VerilatorVersion};

/// Version of the C ABI between generated Rust and the C++ runtime. It is part of every
/// cache key, so changing the ABI invalidates every cached model and runtime. The C ABI
/// runtime owns its value; bump it whenever the ABI changes.
pub const ABI_VERSION: u32 = 1;
