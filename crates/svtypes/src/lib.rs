//! Type IR for elaborated SystemVerilog designs.
//!
//! Reads Verilator's `--json-only` output and produces a stable description of types,
//! widths, enums, parameters, ports and interfaces that back ends turn into Rust or Python.
//!
//! - [`ir`]: the versioned IR ([`ir::Design`] and friends).
//! - [`verilator::json_pass`]: run Verilator and decode its output in one call.
//! - [`frontend::parse`]: decode already-written `.tree.json` / `.tree.meta.json` files.
//! - [`literal`] and [`width`]: the literal parser and width calculator the front ends use.
#![forbid(unsafe_code)]

pub mod error;
pub mod frontend;
pub mod ir;
pub mod literal;
pub mod verilator;
pub mod width;
