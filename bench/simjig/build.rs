//! Verilates `examples/lib/rtl/sync_fifo.sv` (8 bits by 16 entries, registered read) for
//! the benchmark, twice, as a sweep:
//! - `fifo`: simjig's own flags (ports only are reachable);
//! - `fifo_pub`: the same plus `--public-flat-rw`, which cocotb's Verilator flow always
//!   passes (VPI needs it). Comparing the two isolates what that flag costs the model.
//!
//! No tracing, no coverage, no `--timing`: the cocotb build has none of them either.
//! `BENCH_RTL` replaces the RTL file (`run.sh --bug` points it at a copy with an injected
//! bug), so the same testbench can be shown to catch it.

use simjig_build::{Dut, Error, Sweep};

fn main() -> Result<(), Error> {
    println!("cargo::rerun-if-env-changed=BENCH_RTL");
    let rtl = std::env::var("BENCH_RTL")
        .unwrap_or_else(|_| "../../examples/lib/rtl/sync_fifo.sv".to_owned());
    let fifo = |name: &str| {
        Dut::new(name)
            .sources([rtl.as_str()])
            .top("sync_fifo")
            .param("WIDTH", 8)
            .param("DEPTH", 16)
            .param("FWFT", 0)
    };
    Sweep::new("bench_fifo")
        .dut(fifo("fifo"))
        .dut(fifo("fifo_pub").verilator_args(["--public-flat-rw"]))
        .build()?;
    Ok(())
}
