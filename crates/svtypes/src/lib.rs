//! Type IR for elaborated SystemVerilog designs.
//!
//! Reads Verilator's `--json-only` output and produces a stable description of types,
//! widths, enums, parameters, ports and interfaces that back ends turn into Rust or Python.
#![forbid(unsafe_code)]
