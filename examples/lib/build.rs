//! Verilates each library module, generates its Rust bindings and C++ glue, and links the
//! model into this crate's tests (`simjig::include_dut!(<name>)` in a test file).
//!
//! One function per DUT, each in its own block, so modules can be added independently.
//! Every DUT is linted with `-Wall`; Verilator's warnings fail the build. Every DUT is
//! built with `.trace_fst(true)`, so any failing test here can be replayed with waves
//! (`SIMJIG_WAVES_ON_FAIL=1`, or the report's `waves:` line). Tracing costs only build
//! time: a run is traced only when it is replayed for waves.

use simjig_build::{Dut, Error, IfaceBinding};

/// Verilator arguments shared by every DUT of this crate.
const LINT: [&str; 1] = ["-Wall"];

fn main() -> Result<(), Error> {
    sync_fifo()?;
    skid_buffer()?;
    round_robin_arbiter()?;
    async_fifo()?;
    Ok(())
}

// ---- sync_fifo (rtl/sync_fifo.sv, rtl/item_fifo.sv, rtl/fifo_pkg.sv) --------------------

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
        .trace_fst(true)
        .verilator_args(LINT)
        .build()?;
    Dut::new("item_fifo")
        .sources(["rtl/fifo_pkg.sv", "rtl/sync_fifo.sv", "rtl/item_fifo.sv"])
        .top("item_fifo")
        .param("DEPTH", 4)
        .param("FWFT", 1)
        .trace_fst(true)
        .verilator_args(LINT)
        .build()?;
    Ok(())
}

// ---- skid_buffer (rtl/skid_buffer.sv, rtl/stream_if.sv) ---------------------------------

/// Beat width of the `skid_buffer` model.
const SKID_WIDTH: u32 = 16;

/// `skid_buffer` with 16-bit beats. Its ports are `stream_if` modports, which a Verilator
/// top cannot have, so the generated wrapper instantiates one `stream_if` per port. The
/// interface's `WIDTH` is its own parameter: `.param` sets only the module's, so each
/// binding passes it too (`bind_iface` alone would give the interface its default, 8).
fn skid_buffer() -> Result<(), Error> {
    let stream = |port: &str, modport: &str| IfaceBinding {
        port: port.into(),
        interface: "stream_if".into(),
        params: vec![("WIDTH".into(), SKID_WIDTH.to_string())],
        modport: Some(modport.into()),
    };
    Dut::new("skid_buffer")
        .sources(["rtl/stream_if.sv", "rtl/skid_buffer.sv"])
        .top("skid_buffer")
        .param("WIDTH", SKID_WIDTH)
        .bind_iface_with(stream("up", "sink"))
        .bind_iface_with(stream("down", "source"))
        .trace_fst(true)
        .verilator_args(LINT)
        // The generated top (`simjig_wrap_skid_buffer`) declares no `timeunit`, while every
        // RTL unit here does: without a default, Verilator stops on the mix (TIMESCALEMOD).
        .verilator_args(["--timescale", "1ns/1ps"])
        .build()?;
    Ok(())
}

// ---- round_robin_arbiter (rtl/round_robin_arbiter.sv) -----------------------------------

/// The arbiter with five requesters: a count that is not a power of 2, so the pointer's
/// wrap from 4 to 0 is exercised.
fn round_robin_arbiter() -> Result<(), Error> {
    Dut::new("round_robin_arbiter")
        .sources(["rtl/round_robin_arbiter.sv"])
        .top("round_robin_arbiter")
        .param("N", 5)
        .trace_fst(true)
        .verilator_args(LINT)
        .build()?;
    Ok(())
}

// ---- async_fifo (rtl/async_fifo.sv) -----------------------------------------------------

/// The dual-clock FIFO, 8 entries of 8 bits: shallow, so random traffic reaches full often.
fn async_fifo() -> Result<(), Error> {
    Dut::new("async_fifo")
        .sources(["rtl/async_fifo.sv"])
        .top("async_fifo")
        .param("WIDTH", 8)
        .param("DEPTH", 8)
        .trace_fst(true)
        .verilator_args(LINT)
        .build()?;
    Ok(())
}
