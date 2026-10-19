//! `tests/conformance/tb/test_flop.py`.

use simjig::{ns, read_only, read_write, timer, with_timeout};

use crate::conflog::{Log, Probe};
use crate::flop::Dut;
use crate::start_clock;

/// `start_clock()`.
fn start(dut: Dut) {
    dut.clk.set(0);
    start_clock(dut.clk, 10);
}

/// `reset()`.
async fn reset(p: &mut Probe, dut: Dut) {
    p.write(&dut.rst, 1);
    p.write(&dut.d, 0);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);
}

#[simjig::test]
async fn edge_read(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    p.read(&[&dut.clk, &dut.rst, &dut.d, &dut.q]);
    start(dut);
    reset(&mut p, dut).await;
    for v in [1, 0, 1, 1, 0] {
        p.write(&dut.d, v);
        p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
        p.read(&[&dut.clk, &dut.d, &dut.q]);
        p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
        p.read(&[&dut.clk, &dut.d, &dut.q]);
    }
    log.check("edge_read");
}

#[simjig::test]
async fn readonly_read(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    start(dut);
    reset(&mut p, dut).await;
    for v in [1, 0, 1] {
        p.write(&dut.d, v);
        p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
        p.read(&[&dut.d, &dut.q]);
        p.wait(read_only(), "ReadOnly").await;
        p.read(&[&dut.clk, &dut.d, &dut.q]);
        p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    }
    log.check("readonly_read");
}

#[simjig::test]
async fn write_after_edge(dut: Dut) {
    let (d, q) = (dut.d, dut.q);
    let log = Log::default();
    let mut p = log.probe("main");
    start(dut);
    reset(&mut p, dut).await;
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.read(&[&d, &q]);
    p.write(&d, 1);
    p.read(&[&d, &q]);
    p.wait(read_write(), "ReadWrite").await;
    p.read(&[&d, &q]);
    p.wait(read_only(), "ReadOnly").await;
    p.read(&[&d, &q]);
    p.wait(timer(ns(1)), "Timer(1ns)").await;
    p.read(&[&d, &q]);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.read(&[&d, &q]);
    p.write(&d, 0);
    p.write(&d, 1);
    p.wait(read_only(), "ReadOnly").await;
    p.read(&[&d, &q]);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.read(&[&d, &q]);
    log.check("write_after_edge");
}

#[simjig::test]
async fn timeout(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    start(dut);
    reset(&mut p, dut).await;
    for t in [3, 5, 10] {
        let outcome = with_timeout(ns(t), dut.clk.rising()).await;
        p.woke(&format!("with_timeout(RisingEdge(clk),{t}ns)"));
        let outcome = if outcome.is_ok() {
            "fired"
        } else {
            "SimTimeoutError"
        };
        p.note("outcome", outcome);
        p.read(&[&dut.clk, &dut.q]);
    }
    log.check("timeout");
}
