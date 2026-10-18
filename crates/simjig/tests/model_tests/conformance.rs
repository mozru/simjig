//! The cocotb conformance scenarios against **real** Verilator models of the DUTs, through
//! the typed handles, diffed byte for byte with `tests/conformance/golden/*.jsonl` (cocotb
//! 2.1.0 on Verilator 5.052). Each scenario mirrors its `tests/conformance/tb/test_<dut>.py`
//! function line by line, as `tests/kernel_tests/conformance.rs` does on `FakeModel`
//! replicas; here the model is the DUT itself.

use std::future::Future;

use simjig::{
    ReadOnly, ReadWrite, Sim, SimDuration, Timer, cycles, first, join, ns, spawn, with_timeout,
};

use super::conflog::{Log, Probe};
use super::duts::{comb_chain, counter, flop};
use super::start_clock;

fn flop_sim() -> (Sim, flop::Dut) {
    let (mut sim, dut) = flop::Dut::sim().unwrap();
    start_clock(&mut sim, dut.clk.raw());
    (sim, dut)
}

fn counter_sim() -> (Sim, counter::Dut) {
    let (mut sim, dut) = counter::Dut::sim().unwrap();
    start_clock(&mut sim, dut.clk.raw());
    (sim, dut)
}

/// Runs `main` and compares the log with the golden `name`.
fn check(name: &str, mut sim: Sim, log: &Log, main: impl Future<Output = ()> + 'static) {
    let path = format!(
        "{}/../../tests/conformance/golden/{name}.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let golden = std::fs::read_to_string(&path).unwrap();
    sim.run(main).unwrap_or_else(|f| panic!("{name}: {f}"));
    let ours = log.text();
    if ours != golden {
        let first = ours
            .lines()
            .zip(golden.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(ours.lines().count().min(golden.lines().count()));
        panic!(
            "{name} differs from the golden at line {}:\n  ours:   {}\n  golden: {}\n--- ours ---\n{ours}",
            first + 1,
            ours.lines().nth(first).unwrap_or("<end>"),
            golden.lines().nth(first).unwrap_or("<end>"),
        );
    }
}

/// `reset()` from `test_flop.py`.
async fn flop_reset(p: &mut Probe, dut: flop::Dut) {
    p.write(&dut.rst, 1);
    p.write(&dut.d, 0);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);
}

#[test]
fn edge_read() {
    let (sim, dut) = flop_sim();
    let log = Log::default();
    let mut p = log.probe("main");
    check("edge_read", sim, &log, async move {
        p.read(&[&dut.clk, &dut.rst, &dut.d, &dut.q]);
        dut.clk.set(0);
        flop_reset(&mut p, dut).await;
        for v in [1, 0, 1, 1, 0] {
            p.write(&dut.d, v);
            p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
            p.read(&[&dut.clk, &dut.d, &dut.q]);
            p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
            p.read(&[&dut.clk, &dut.d, &dut.q]);
        }
    });
}

#[test]
fn readonly_read() {
    let (sim, dut) = flop_sim();
    let log = Log::default();
    let mut p = log.probe("main");
    check("readonly_read", sim, &log, async move {
        dut.clk.set(0);
        flop_reset(&mut p, dut).await;
        for v in [1, 0, 1] {
            p.write(&dut.d, v);
            p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
            p.read(&[&dut.d, &dut.q]);
            p.wait(ReadOnly::new(), "ReadOnly").await;
            p.read(&[&dut.clk, &dut.d, &dut.q]);
            p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
        }
    });
}

#[test]
fn write_after_edge() {
    let (sim, dut) = flop_sim();
    let (d, q) = (dut.d, dut.q);
    let log = Log::default();
    let mut p = log.probe("main");
    check("write_after_edge", sim, &log, async move {
        dut.clk.set(0);
        flop_reset(&mut p, dut).await;
        p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
        p.read(&[&d, &q]);
        p.write(&d, 1);
        p.read(&[&d, &q]);
        p.wait(ReadWrite::new(), "ReadWrite").await;
        p.read(&[&d, &q]);
        p.wait(ReadOnly::new(), "ReadOnly").await;
        p.read(&[&d, &q]);
        p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
        p.read(&[&d, &q]);
        p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
        p.read(&[&d, &q]);
        p.write(&d, 0);
        p.write(&d, 1);
        p.wait(ReadOnly::new(), "ReadOnly").await;
        p.read(&[&d, &q]);
        p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
        p.read(&[&d, &q]);
    });
}

#[test]
fn timeout() {
    let (sim, dut) = flop_sim();
    let log = Log::default();
    let mut p = log.probe("main");
    check("timeout", sim, &log, async move {
        dut.clk.set(0);
        flop_reset(&mut p, dut).await;
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
    });
}

/// `start()` from `test_counter.py`.
async fn counter_start(p: &mut Probe, dut: counter::Dut) {
    dut.clk.set(0);
    p.write(&dut.rst, 1);
    p.write(&dut.en, 1);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);
}

#[test]
fn timer() {
    let (sim, dut) = counter_sim();
    let log = Log::default();
    let mut p = log.probe("main");
    check("timer", sim, &log, async move {
        counter_start(&mut p, dut).await;
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
    });
}

#[test]
fn clock_cycles() {
    let (sim, dut) = counter_sim();
    let (clk, count) = (dut.clk, dut.count);
    let log = Log::default();
    let mut p = log.probe("main");
    check("clock_cycles", sim, &log, async move {
        counter_start(&mut p, dut).await;
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
    });
}

#[test]
fn value_change() {
    let (sim, dut) = counter_sim();
    let (clk, en, count) = (dut.clk, dut.en, dut.count);
    let log = Log::default();
    let mut p = log.probe("main");
    let mut w = log.probe("en_watch");
    check("value_change", sim, &log, async move {
        counter_start(&mut p, dut).await;
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
    });
}

#[test]
fn first_combine() {
    let (sim, dut) = counter_sim();
    let (clk, count) = (dut.clk, dut.count);
    let log = Log::default();
    let mut p = log.probe("main");
    check("first_combine", sim, &log, async move {
        counter_start(&mut p, dut).await;
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
    });
}

#[test]
fn comb_settle() {
    let (sim, dut) = comb_chain::Dut::sim().unwrap();
    let comb_chain::Dut {
        a,
        b,
        y,
        y_eq,
        s1,
        s2,
        s3,
    } = dut;
    let log = Log::default();
    let mut p = log.probe("main");
    let mut w = log.probe("y_watch");
    check("comb_settle", sim, &log, async move {
        p.write(&a, 0);
        p.write(&b, 0);
        p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
        p.read(&[&a, &b, &y, &y_eq]);
        let watcher = spawn("y_watch", async move {
            for _ in 0..3 {
                w.wait(y.changed(), "ValueChange(y)").await;
                w.read(&[&a, &y]);
            }
        });
        for (va, vb) in [(0x10, 0), (0x20, 3), (0xff, 0x40)] {
            p.write(&a, va);
            p.write(&b, vb);
            p.read(&[&a, &y]);
            p.wait(ReadWrite::new(), "ReadWrite").await;
            p.read(&[&a, &y]);
            p.wait(ReadOnly::new(), "ReadOnly").await;
            p.read(&[&a, &b, &y, &y_eq, &s1, &s2, &s3]);
            p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
            p.read(&[&a, &y]);
        }
        p.wait(ReadWrite::new(), "ReadWrite").await;
        p.write(&a, 0x01);
        p.read(&[&a, &y]);
        p.wait(ReadOnly::new(), "ReadOnly").await;
        p.read(&[&a, &y]);
        watcher.await;
    });
}
