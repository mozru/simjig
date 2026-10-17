//! Build-time support: runs Verilator, generates the C++ shim and the typed Rust DUT.
//!
//! Called from a user crate's `build.rs`.
#![forbid(unsafe_code)]
