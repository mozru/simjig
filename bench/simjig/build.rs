//! Verilates the benchmark's two designs (`bench/README.md`, "Designs"), each twice, as a
//! sweep:
//! - `bench_fifo`: `examples/lib/rtl/sync_fifo.sv` (8 bits by 16 entries, registered
//!   read), as `fifo` and `fifo_pub`;
//! - `bench_mid`: `bench/rtl/mid_pipe.sv` (a 64-bit FIFO of 256 entries feeding a
//!   128-stage pipeline; its parameters are its defaults), as `mid` and `mid_pub`.
//!
//! The plain member has simjig's own flags (ports only are reachable); `_pub` adds
//! `--public-flat-rw`, which cocotb's Verilator flow always passes (VPI needs it).
//! Comparing the two isolates what that flag costs the model.
//!
//! No tracing, no coverage, no `--timing`: the cocotb build has none of them either.
//! `BENCH_RTL` replaces the FIFO's RTL file and `BENCH_MID_RTL` the mid-size design's
//! (`bench.py bug` points one at a copy with an injected bug), so the same testbench can
//! be shown to catch it.

use simjig_build::{Dut, Error, Sweep};

fn main() -> Result<(), Error> {
    println!("cargo::rerun-if-env-changed=BENCH_RTL");
    println!("cargo::rerun-if-env-changed=BENCH_MID_RTL");
    let fifo_rtl = std::env::var("BENCH_RTL")
        .unwrap_or_else(|_| "../../examples/lib/rtl/sync_fifo.sv".to_owned());
    let mid_rtl =
        std::env::var("BENCH_MID_RTL").unwrap_or_else(|_| "../rtl/mid_pipe.sv".to_owned());
    let fifo = |name: &str| {
        Dut::new(name)
            .sources([fifo_rtl.as_str()])
            .top("sync_fifo")
            .param("WIDTH", 8)
            .param("DEPTH", 16)
            .param("FWFT", 0)
    };
    Sweep::new("bench_fifo")
        .dut(fifo("fifo"))
        .dut(fifo("fifo_pub").verilator_args(["--public-flat-rw"]))
        .build()?;
    let mid = |name: &str| {
        Dut::new(name)
            .sources([mid_rtl.as_str(), "../../examples/lib/rtl/sync_fifo.sv"])
            .top("mid_pipe")
    };
    Sweep::new("bench_mid")
        .dut(mid("mid"))
        .dut(mid("mid_pub").verilator_args(["--public-flat-rw"]))
        .build()?;
    Ok(())
}
