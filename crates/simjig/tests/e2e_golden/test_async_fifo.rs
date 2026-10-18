//! `tests/conformance/tb/test_async_fifo.py` (`DEPTH=8`): two clocks whose rising edges
//! coincide. `wbin`, `rgray_w2`, `rbin` and `wgray_r2` are internal, read through probes.

use simjig::{ReadOnly, Timer, cycles, join, ns, spawn};
use simjig_bits::Bit;

use crate::async_fifo::Dut;
use crate::conflog::{Log, Probe};
use crate::start_clock;

/// `start_clocks()`: wclk 10 ns, rclk 30 ns, both started low, wclk first.
fn start_clocks(dut: Dut) {
    dut.wclk.set(0);
    dut.rclk.set(0);
    start_clock(dut.wclk, 10);
    start_clock(dut.rclk, 30);
}

/// `reset()`: both resets for two rclk cycles, released at the 60 ns falling edge.
async fn reset(dut: Dut, p: &mut Probe) {
    p.write(&dut.wrst, 1);
    p.write(&dut.rrst, 1);
    p.write(&dut.winc, 0);
    p.write(&dut.wdata, 0);
    p.write(&dut.rinc, 0);
    p.wait(cycles(dut.rclk.raw(), 2), "ClockCycles(rclk,2)")
        .await;
    p.wait(dut.rclk.falling(), "FallingEdge(rclk)").await;
    p.write(&dut.wrst, 0);
    p.write(&dut.rrst, 0);
}

#[simjig::test]
async fn two_clocks(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let mut pw = log.probe("wclk_task");
    let mut pr = log.probe("rclk_task");
    let mut pc = log.probe("both_task");
    start_clocks(dut);
    reset(dut, &mut p).await;

    let wclk_waiter = async move {
        for _ in 0..7 {
            pw.wait(dut.wclk.rising(), "RisingEdge(wclk)").await;
            pw.read(&[&dut.wclk, &dut.rclk, &dut.wbin, &dut.rgray_w2, &dut.wfull]);
        }
    };
    let rclk_waiter = async move {
        for _ in 0..3 {
            pr.wait(dut.rclk.rising(), "RisingEdge(rclk)").await;
            pr.read(&[&dut.wclk, &dut.rclk, &dut.rbin, &dut.wgray_r2, &dut.rempty]);
        }
    };
    let both_waiter = async move {
        pc.wait(
            async { join!(dut.wclk.rising(), dut.rclk.rising()) },
            "Combine(RisingEdge(wclk),RisingEdge(rclk))",
        )
        .await;
        pc.read(&[&dut.wclk, &dut.rclk, &dut.wbin, &dut.rbin]);
    };
    let tw = spawn("wclk_task", wclk_waiter);
    let tr = spawn("rclk_task", rclk_waiter);
    let tc = spawn("both_task", both_waiter);
    for value in [0xA1, 0xB2, 0xC3] {
        p.wait(dut.wclk.falling(), "FallingEdge(wclk)").await;
        p.write(&dut.winc, 1);
        p.write(&dut.wdata, value);
    }
    p.wait(dut.wclk.falling(), "FallingEdge(wclk)").await;
    p.write(&dut.winc, 0);
    tw.await;
    tr.await;
    tc.await;
    log.check("two_clocks");
}

#[simjig::test]
async fn cdc_stream(dut: Dut) {
    let log = Log::default();
    let mut pw = log.probe("writer");
    let mut pr = log.probe("reader");
    start_clocks(dut);
    reset(dut, &mut pw).await;
    let data: Vec<u128> = (0..8).map(|i| 0x10 + i).collect();

    let writer = async move {
        let mut i = 0;
        while i < data.len() {
            pw.wait(dut.wclk.falling(), "FallingEdge(wclk)").await;
            pw.wait(ReadOnly::new(), "ReadOnly").await;
            pw.read(&[&dut.wfull]);
            let full = dut.wfull.get() == Bit::ONE;
            pw.wait(Timer::new(ns(1)), "Timer(1ns)").await;
            pw.write(&dut.winc, u128::from(!full));
            pw.write(&dut.wdata, data[i]);
            i += usize::from(!full);
        }
        pw.wait(dut.wclk.falling(), "FallingEdge(wclk)").await;
        pw.write(&dut.winc, 0);
    };
    let reader = async move {
        let mut got = 0;
        while got < 8 {
            pr.wait(dut.rclk.falling(), "FallingEdge(rclk)").await;
            pr.wait(ReadOnly::new(), "ReadOnly").await;
            pr.read(&[&dut.rempty, &dut.rdata]);
            let empty = dut.rempty.get() == Bit::ONE;
            pr.wait(Timer::new(ns(1)), "Timer(1ns)").await;
            pr.write(&dut.rinc, u128::from(!empty));
            got += usize::from(!empty);
        }
        pr.wait(dut.rclk.falling(), "FallingEdge(rclk)").await;
        pr.write(&dut.rinc, 0);
    };
    let w = spawn("writer", writer);
    let r = spawn("reader", reader);
    w.await;
    r.await;
    log.check("cdc_stream");
}
