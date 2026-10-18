//! Verilates each library module, generates its Rust bindings and C++ glue, and links the
//! model into this crate's tests (`simjig::include_dut!(<name>)` in a test file).
//!
//! One function per DUT, each in its own block, so modules can be added independently.
//! Every DUT is linted with `-Wall`; Verilator's warnings fail the build. Every DUT is
//! built with `.trace_fst(true)`, so any failing test here can be replayed with waves
//! (`SIMJIG_WAVES_ON_FAIL=1`, or the report's `waves:` line). Tracing costs only build
//! time: a run is traced only when it is replayed for waves.

use simjig_build::{Dut, Error, Sweep};

/// Verilator arguments shared by every DUT of this crate.
const LINT: [&str; 1] = ["-Wall"];

fn main() -> Result<(), Error> {
    sync_fifo()?;
    skid_buffer()?;
    round_robin_arbiter()?;
    async_fifo()?;
    hop_stage()?;
    Ok(())
}

// ---- sync_fifo (rtl/sync_fifo.sv, rtl/item_fifo.sv, rtl/fifo_pkg.sv) --------------------

/// `sync_fifo` as a parameter sweep of two raw FIFOs with a registered read, 8 bits by 4
/// entries and 32 bits by 16, so each test is written once for both
/// (`simjig::include_dut!(sync_fifo)`, `sync_fifo::Ports`); and `item_fifo`, the same FIFO
/// over the packed struct `fifo_pkg::item_t` in first-word-fall-through mode. Each
/// parameter set is its own model.
fn sync_fifo() -> Result<(), Error> {
    let fifo = |name: &str, width: u32, depth: u32| {
        Dut::new(name)
            .sources(["rtl/sync_fifo.sv"])
            .top("sync_fifo")
            .param("WIDTH", width)
            .param("DEPTH", depth)
            .param("FWFT", 0)
            .trace_fst(true)
            .verilator_args(LINT)
    };
    Sweep::new("sync_fifo")
        .dut(fifo("fifo_8x4", 8, 4))
        .dut(fifo("fifo_32x16", 32, 16))
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
/// interface's `WIDTH` follows the module's `.param("WIDTH", ..)`, which simjig passes to
/// each bound interface that declares a parameter of that name.
fn skid_buffer() -> Result<(), Error> {
    Dut::new("skid_buffer")
        .sources(["rtl/stream_if.sv", "rtl/skid_buffer.sv"])
        .top("skid_buffer")
        .param("WIDTH", SKID_WIDTH)
        .bind_iface("up", "stream_if.sink")
        .bind_iface("down", "stream_if.source")
        .trace_fst(true)
        .verilator_args(LINT)
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

// ---- hop_stage (rtl/hop_stage.sv, rtl/tagged_if.sv) -------------------------------------

/// `hop_stage` as a sweep of 8-bit beats with 2-bit tags and 32-bit beats with 4-bit tags:
/// the interface's `beat_t` has different field widths in each, so a test generic over
/// `hop_stage::Ports` reads and builds it through `hop_stage::BeatFields`. The interfaces'
/// `WIDTH` and `TAG_W` follow the module's `.param`s.
fn hop_stage() -> Result<(), Error> {
    let stage = |name: &str, width: u32, tag_w: u32| {
        Dut::new(name)
            .sources(["rtl/tagged_if.sv", "rtl/hop_stage.sv"])
            .top("hop_stage")
            .param("WIDTH", width)
            .param("TAG_W", tag_w)
            .bind_iface("up", "tagged_if.sink")
            .bind_iface("down", "tagged_if.source")
            .trace_fst(true)
            .verilator_args(LINT)
    };
    Sweep::new("hop_stage")
        .dut(stage("hop_8x2", 8, 2))
        .dut(stage("hop_32x4", 32, 4))
        .build()?;
    Ok(())
}
