//! The `cargo simjig` tool.
//!
//! The binary is invoked as `cargo simjig <command>`. Its logic lives in this library so
//! that `xtask` can run the same checks (`cargo xtask doctor`) without a second copy.
//!
//! - [`doctor`] checks that Verilator, the C++ toolchain and the test runner are usable
//!   and pinned, and says how to fix each problem.
//! - [`verilator`] holds the table of supported Verilator versions.

#![forbid(unsafe_code)]

pub mod doctor;
mod json;
pub mod verilator;
