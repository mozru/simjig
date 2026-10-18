//! Verilates each library module, generates its Rust bindings and C++ glue, and links the
//! model into this crate's tests (`simjig::include_dut!(<name>)` in a test file).
//!
//! One function per DUT, each in its own block, so modules can be added independently.
//! Every DUT is linted with `-Wall`; Verilator's warnings fail the build.

use simjig_build::{Dut, Error};

/// Verilator arguments shared by every DUT of this crate.
const LINT: [&str; 1] = ["-Wall"];

fn main() -> Result<(), Error> {
    sync_fifo()?;
    Ok(())
}

// ---- sync_fifo (rtl/sync_fifo.sv, rtl/item_fifo.sv, rtl/fifo_pkg.sv) -------------------

/// `sync_fifo` twice: a raw 8-bit, 4-deep FIFO with a registered read (`sync_fifo`), and
/// `item_fifo`, the same FIFO over the packed struct `fifo_pkg::item_t` in
/// first-word-fall-through mode. Each parameter set is its own model.
fn sync_fifo() -> Result<(), Error> {
    Dut::new("sync_fifo")
        .sources(["rtl/sync_fifo.sv"])
        .top("sync_fifo")
        .param("WIDTH", 8)
        .param("DEPTH", 4)
        .param("FWFT", 0)
        .verilator_args(LINT)
        .build()?;
    Dut::new("item_fifo")
        .sources(["rtl/fifo_pkg.sv", "rtl/sync_fifo.sv", "rtl/item_fifo.sv"])
        .top("item_fifo")
        .param("DEPTH", 4)
        .param("FWFT", 1)
        .verilator_args(LINT)
        .build()?;
    Ok(())
}
