//! The `cargo simjig` tool.
//!
//! The binary is invoked as `cargo simjig <command>`. Its logic lives in this library so
//! that `xtask` can run the same checks (`cargo xtask doctor`) without a second copy.
//!
//! - [`doctor`] checks that Verilator, the C++ toolchain and the test runner are usable
//!   and pinned, and says how to fix each problem.
//! - [`waves`] runs one test with `SIMJIG_WAVES`, so its harness writes waves from a
//!   deterministic replay.
//! - [`show`] prints the Rust module simjig-build generated for a DUT (what
//!   `simjig::include_dut!` includes), or its path.
//!
//! The supported Verilator versions are `svtypes::version::SUPPORTED`, the table every
//! build-time crate shares.

#![forbid(unsafe_code)]

pub mod doctor;
mod json;
pub mod show;
pub mod waves;
