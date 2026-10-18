//! Type IR for elaborated SystemVerilog designs.
//!
//! Reads Verilator's `--json-only` output and produces a stable description of types,
//! widths, enums, parameters, ports and interfaces that back ends turn into Rust or Python.
//!
//! - [`ir`]: the versioned IR ([`ir::Design`] and friends).
//! - [`verilator::json_pass`]: run Verilator and decode its output in one call.
//! - [`frontend::parse`]: decode already-written `.tree.json` / `.tree.meta.json` files.
//! - [`version`]: the supported Verilator versions and their parser.
//! - [`toolchain`]: which Verilator to run and whether to accept it (build and doctor).
//! - [`literal`] and [`width`]: the literal parser and width calculator the front ends use.

pub mod error;
pub mod frontend;
pub mod ir;
pub mod literal;
pub mod toolchain;
pub mod verilator;
pub mod version;
pub mod width;
