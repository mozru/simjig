//! `tests/conformance/tb/test_counter.py`.

use simjig::{ReadOnly, SimDuration, Timer, cycles, first, join, ns, spawn};

use crate::conflog::{Log, Probe};
use crate::counter::Dut;
use crate::start_clock;

/// `start()`.
async fn start(p: &mut Probe, dut: Dut) {
    dut.clk.set(0);
    start_clock(dut.clk, 10);
    p.write(&dut.rst, 1);
    p.write(&dut.en, 1);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);
}

#[simjig::test]
async fn timer(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    start(&mut p, dut).await;
    for delay in [3, 2, 7, 3, 10, 10] {
        p.wait(Timer::new(ns(delay)), &format!("Timer({delay}ns)"))
            .await;
        p.read(&[&dut.clk, &dut.count]);
        p.wait(ReadOnly::new(), "ReadOnly").await;
        p.read(&[&dut.clk, &dut.count]);
    }
    p.wait(Timer::new(SimDuration::from_ticks(1)), "Timer(1step)")
        .await;
    p.read(&[&dut.clk, &dut.count]);
    log.check("timer");
}

#[simjig::test]
async fn clock_cycles(dut: Dut) {
    let (clk, count) = (dut.clk, dut.count);
    let log = Log::default();
    let mut p = log.probe("main");
    start(&mut p, dut).await;
    p.wait(cycles(clk.raw(), 3), "ClockCycles(clk,3)").await;
    p.read(&[&clk, &count]);
    let two_falling = async move {
        for _ in 0..2 {
            clk.falling().await;
        }
    };
    p.wait(two_falling, "ClockCycles(clk,2,falling)").await;
    p.read(&[&clk, &count]);
    p.wait(cycles(clk.raw(), 1), "ClockCycles(clk,1)").await;
    p.read(&[&clk, &count]);
    log.check("clock_cycles");
}

#[simjig::test]
async fn value_change(dut: Dut) {
    let (clk, en, count) = (dut.clk, dut.en, dut.count);
    let log = Log::default();
    let mut p = log.probe("main");
    let mut w = log.probe("en_watch");
    start(&mut p, dut).await;
    let watcher = spawn("en_watch", async move {
        for _ in 0..2 {
            w.wait(en.changed(), "ValueChange(en)").await;
            w.read(&[&en, &count]);
        }
    });
    for _ in 0..3 {
        p.wait(count.changed(), "ValueChange(count)").await;
        p.read(&[&clk, &count]);
    }
    p.wait(clk.falling(), "FallingEdge(clk)").await;
    p.write(&en, 0);
    p.wait(Timer::new(ns(20)), "Timer(20ns)").await;
    p.write(&en, 1);
    p.wait(count.changed(), "ValueChange(count)").await;
    p.read(&[&clk, &count]);
    watcher.await;
    log.check("value_change");
}

#[simjig::test]
async fn first_combine(dut: Dut) {
    let (clk, count) = (dut.clk, dut.count);
    let log = Log::default();
    let mut p = log.probe("main");
    start(&mut p, dut).await;
    for t in [3, 8, 10] {
        let won = first!(
            async move {
                clk.rising().await;
                "RisingEdge(clk)".to_owned()
            },
            async move {
                Timer::new(ns(t)).await;
                format!("Timer({t}ns)")
            },
        );
        p.woke(&format!("First(RisingEdge(clk),Timer({t}ns))"));
        p.note("first", &won);
        p.read(&[&clk, &count]);
    }
    let label = "Combine(ClockCycles(clk,2),Timer(13ns))";
    p.wait(
        async { join!(cycles(clk.raw(), 2), Timer::new(ns(13))) },
        label,
    )
    .await;
    p.read(&[&clk, &count]);
    let label = "Combine(RisingEdge(clk),Timer(1ns))";
    p.wait(async { join!(clk.rising(), Timer::new(ns(1))) }, label)
        .await;
    p.read(&[&clk, &count]);
    log.check("first_combine");
}
