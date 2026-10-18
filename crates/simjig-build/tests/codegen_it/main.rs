//! Integration tests of `simjig_build::codegen`: snapshots of every emitter, the generated
//! Rust compiled against a stub runtime, and Verilator runs of the generated SV and C++.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] fns; a panic is the failure report"
)]

// Generated code names `::simjig::__private`; here that is the stub below.
extern crate self as simjig;

// The generated modules are reachable (`compile` is `pub`), as they are in a user crate
// that `include_dut!`s them, so the generated code is checked under the workspace lints
// without exceptions. The hand-written stub and case list are not; they are exempt.
#[path = "stub.rs"]
#[expect(
    unreachable_pub,
    reason = "a stand-in for the runtime's hidden module, written as for a library"
)]
mod __private;
#[expect(unreachable_pub, reason = "test helpers written as for a library")]
mod cases;
pub mod compile;
mod coverage;
mod dpi;
mod errors;
mod probes;
mod snapshots;
mod verilator;
