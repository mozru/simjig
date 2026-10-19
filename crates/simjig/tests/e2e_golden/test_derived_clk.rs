//! `tests/conformance/tb/test_derived_clk.py`: triggers on a design-driven divided clock.

use simjig::{cycles, read_only, spawn};

use crate::conflog::Log;
use crate::derived_clk::Dut;
use crate::start_clock;

#[simjig::test]
async fn derived_edge(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let mut f = log.probe("fast");
    dut.clk.set(0);
    start_clock(dut.clk, 10);
    p.write(&dut.rst, 1);
    p.wait(cycles(dut.clk.raw(), 2), "ClockCycles(clk,2)").await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);

    let fast = async move {
        for _ in 0..6 {
            f.wait(dut.clk.rising(), "RisingEdge(clk)").await;
            f.read(&[&dut.clk, &dut.div_clk, &dut.fast_count, &dut.slow_count]);
        }
    };
    let t = spawn("fast", fast);
    for _ in 0..3 {
        p.wait(dut.div_clk.rising(), "RisingEdge(div_clk)").await;
        p.read(&[&dut.clk, &dut.div_clk, &dut.fast_count, &dut.slow_count]);
        p.wait(read_only(), "ReadOnly").await;
        p.read(&[&dut.div_clk, &dut.fast_count, &dut.slow_count]);
    }
    t.await;
    log.check("derived_edge");
}
