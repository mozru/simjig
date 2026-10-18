//! Integration tests of `simjig_build::codegen`: snapshots of every emitter, the generated
//! Rust compiled against a stub runtime, and Verilator runs of the generated SV and C++.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] fns; a panic is the failure report"
)]
#![expect(
    unreachable_pub,
    reason = "generated code and the stub runtime are written as for a library crate"
)]

// Generated code names `::simjig::__private`; here that is the stub below.
extern crate self as simjig;

#[path = "stub.rs"]
mod __private;
mod cases;
mod compile;
mod errors;
mod snapshots;
mod verilator;
