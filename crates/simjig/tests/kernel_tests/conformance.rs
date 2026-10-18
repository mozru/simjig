//! The cocotb conformance scenarios, replayed on `FakeModel` replicas of the DUTs and
//! compared byte for byte with the goldens.
//!
//! The goldens in `golden/` are copies of `tests/conformance/golden/*.jsonl` (cocotb 2.1.0
//! on Verilator 5.052, recorded on branch `w1/duts` at commit 2c43e41). Each scenario
//! below mirrors its `tests/conformance/tb/test_<dut>.py` function line by line; the
//! replicas mirror `tests/duts/<dut>/<dut>.sv`. Not replayed here: `two_clocks`,
//! `cdc_stream` and `fifo_stream`, whose DUTs are larger; they belong to the conformance
//! suite against the real Verilator models.

use simjig::{
    Clock, FakeModel, Precision, RawSig, ReadOnly, ReadWrite, Sim, SimDuration, TimeUnit, Timer,
    changed, cycles, falling, first, join, ns, rising, spawn, with_timeout,
};

use super::conflog::Log;

fn sim_with_clock(model: FakeModel) -> (Sim, RawSig) {
    let mut sim = Sim::from_fake(model);
    let clk = sim.signal("clk").unwrap();
    // cocotb `Clock(clk, 10, "ns").start(start_high=False)`: low first, rises at 5 ns.
    let p = sim.precision();
    let clock = Clock::new(clk, p.duration(10, TimeUnit::Ns).unwrap())
        .first_rise_at(p.duration(5, TimeUnit::Ns).unwrap());
    sim.add_clock(clock).unwrap();
    (sim, clk)
}

fn flop() -> (Sim, [RawSig; 4]) {
    let mut m = FakeModel::new(Precision::PS);
    let clk = m.signal("clk", 1);
    let rst = m.signal("rst", 1);
    let d = m.signal("d", 1);
    let q = m.signal("q", 1);
    m.on_rising(clk, move |s| {
        let v = if s.get(rst) == 1 { 0 } else { s.get(d) };
        s.set(q, v);
    });
    let (sim, clk) = sim_with_clock(m);
    let sig = |n| sim.signal(n).unwrap();
    let sigs = [clk, sig("rst"), sig("d"), sig("q")];
    (sim, sigs)
}

fn counter() -> (Sim, [RawSig; 4]) {
    let mut m = FakeModel::new(Precision::PS);
    let clk = m.signal("clk", 1);
    let rst = m.signal("rst", 1);
    let en = m.signal("en", 1);
    let count = m.signal("count", 8);
    m.on_rising(clk, move |s| {
        if s.get(rst) == 1 {
            s.set(count, 0);
        } else if s.get(en) == 1 {
            s.set(count, s.get(count) + 1);
        }
    });
    let (sim, clk) = sim_with_clock(m);
    let sig = |n| sim.signal(n).unwrap();
    let sigs = [clk, sig("rst"), sig("en"), sig("count")];
    (sim, sigs)
}

fn check(
    name: &str,
    golden: &str,
    mut sim: Sim,
    log: &Log,
    main: impl Future<Output = ()> + 'static,
) {
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

use std::future::Future;

/// `reset()` from `test_flop.py`.
async fn flop_reset(p: &mut super::conflog::Probe, [clk, rst, d, _]: [RawSig; 4]) {
    p.write(rst, 1);
    p.write(d, 0);
    p.wait(rising(clk), "RisingEdge(clk)").await;
    p.wait(falling(clk), "FallingEdge(clk)").await;
    p.write(rst, 0);
}

#[test]
fn edge_read() {
    let (sim, sigs) = flop();
    let [clk, rst, d, q] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    check(
        "edge_read",
        include_str!("golden/edge_read.jsonl"),
        sim,
        &log,
        async move {
            p.read(&[clk, rst, d, q]);
            clk.write_u64(0);
            flop_reset(&mut p, sigs).await;
            for v in [1, 0, 1, 1, 0] {
                p.write(d, v);
                p.wait(rising(clk), "RisingEdge(clk)").await;
                p.read(&[clk, d, q]);
                p.wait(falling(clk), "FallingEdge(clk)").await;
                p.read(&[clk, d, q]);
            }
        },
    );
}

#[test]
fn readonly_read() {
    let (sim, sigs) = flop();
    let [clk, _, d, q] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    check(
        "readonly_read",
        include_str!("golden/readonly_read.jsonl"),
        sim,
        &log,
        async move {
            clk.write_u64(0);
            flop_reset(&mut p, sigs).await;
            for v in [1, 0, 1] {
                p.write(d, v);
                p.wait(rising(clk), "RisingEdge(clk)").await;
                p.read(&[d, q]);
                p.wait(ReadOnly::new(), "ReadOnly").await;
                p.read(&[clk, d, q]);
                p.wait(falling(clk), "FallingEdge(clk)").await;
            }
        },
    );
}

#[test]
fn write_after_edge() {
    let (sim, sigs) = flop();
    let [clk, _, d, q] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    let golden = include_str!("golden/write_after_edge.jsonl");
    check("write_after_edge", golden, sim, &log, async move {
        clk.write_u64(0);
        flop_reset(&mut p, sigs).await;
        p.wait(rising(clk), "RisingEdge(clk)").await;
        p.read(&[d, q]);
        p.write(d, 1);
        p.read(&[d, q]);
        p.wait(ReadWrite::new(), "ReadWrite").await;
        p.read(&[d, q]);
        p.wait(ReadOnly::new(), "ReadOnly").await;
        p.read(&[d, q]);
        p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
        p.read(&[d, q]);
        p.wait(rising(clk), "RisingEdge(clk)").await;
        p.read(&[d, q]);
        p.write(d, 0);
        p.write(d, 1);
        p.wait(ReadOnly::new(), "ReadOnly").await;
        p.read(&[d, q]);
        p.wait(rising(clk), "RisingEdge(clk)").await;
        p.read(&[d, q]);
    });
}

#[test]
fn timeout() {
    let (sim, sigs) = flop();
    let [clk, _, _, q] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    check(
        "timeout",
        include_str!("golden/timeout.jsonl"),
        sim,
        &log,
        async move {
            clk.write_u64(0);
            flop_reset(&mut p, sigs).await;
            for t in [3, 5, 10] {
                let outcome = with_timeout(ns(t), rising(clk)).await;
                p.woke(&format!("with_timeout(RisingEdge(clk),{t}ns)"));
                p.note(
                    "outcome",
                    if outcome.is_ok() {
                        "fired"
                    } else {
                        "SimTimeoutError"
                    },
                );
                p.read(&[clk, q]);
            }
        },
    );
}

/// `start()` from `test_counter.py`.
async fn counter_start(p: &mut super::conflog::Probe, [clk, rst, en, _]: [RawSig; 4]) {
    clk.write_u64(0);
    p.write(rst, 1);
    p.write(en, 1);
    p.wait(rising(clk), "RisingEdge(clk)").await;
    p.wait(falling(clk), "FallingEdge(clk)").await;
    p.write(rst, 0);
}

#[test]
fn timer() {
    let (sim, sigs) = counter();
    let [clk, _, _, count] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    check(
        "timer",
        include_str!("golden/timer.jsonl"),
        sim,
        &log,
        async move {
            counter_start(&mut p, sigs).await;
            for delay in [3, 2, 7, 3, 10, 10] {
                p.wait(Timer::new(ns(delay)), &format!("Timer({delay}ns)"))
                    .await;
                p.read(&[clk, count]);
                p.wait(ReadOnly::new(), "ReadOnly").await;
                p.read(&[clk, count]);
            }
            p.wait(Timer::new(SimDuration::from_ticks(1)), "Timer(1step)")
                .await;
            p.read(&[clk, count]);
        },
    );
}

#[test]
fn clock_cycles() {
    let (sim, sigs) = counter();
    let [clk, _, _, count] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    check(
        "clock_cycles",
        include_str!("golden/clock_cycles.jsonl"),
        sim,
        &log,
        async move {
            counter_start(&mut p, sigs).await;
            p.wait(cycles(clk, 3), "ClockCycles(clk,3)").await;
            p.read(&[clk, count]);
            p.wait(
                async move {
                    for _ in 0..2 {
                        falling(clk).await;
                    }
                },
                "ClockCycles(clk,2,falling)",
            )
            .await;
            p.read(&[clk, count]);
            p.wait(cycles(clk, 1), "ClockCycles(clk,1)").await;
            p.read(&[clk, count]);
        },
    );
}

#[test]
fn value_change() {
    let (sim, sigs) = counter();
    let [clk, _, en, count] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    let mut w = log.probe("en_watch");
    check(
        "value_change",
        include_str!("golden/value_change.jsonl"),
        sim,
        &log,
        async move {
            counter_start(&mut p, sigs).await;
            let watcher = spawn("en_watch", async move {
                for _ in 0..2 {
                    w.wait(changed(en), "ValueChange(en)").await;
                    w.read(&[en, count]);
                }
            });
            for _ in 0..3 {
                p.wait(changed(count), "ValueChange(count)").await;
                p.read(&[clk, count]);
            }
            p.wait(falling(clk), "FallingEdge(clk)").await;
            p.write(en, 0);
            p.wait(Timer::new(ns(20)), "Timer(20ns)").await;
            p.write(en, 1);
            p.wait(changed(count), "ValueChange(count)").await;
            p.read(&[clk, count]);
            watcher.await;
        },
    );
}

#[test]
fn first_combine() {
    let (sim, sigs) = counter();
    let [clk, _, _, count] = sigs;
    let log = Log::default();
    let mut p = log.probe("main");
    check(
        "first_combine",
        include_str!("golden/first_combine.jsonl"),
        sim,
        &log,
        async move {
            counter_start(&mut p, sigs).await;
            for t in [3, 8, 10] {
                let won = first!(
                    async move {
                        rising(clk).await;
                        "RisingEdge(clk)".to_owned()
                    },
                    async move {
                        Timer::new(ns(t)).await;
                        format!("Timer({t}ns)")
                    },
                );
                p.woke(&format!("First(RisingEdge(clk),Timer({t}ns))"));
                p.note("first", &won);
                p.read(&[clk, count]);
            }
            let label = "Combine(ClockCycles(clk,2),Timer(13ns))";
            p.wait(async { join!(cycles(clk, 2), Timer::new(ns(13))) }, label)
                .await;
            p.read(&[clk, count]);
            let label = "Combine(RisingEdge(clk),Timer(1ns))";
            p.wait(async { join!(rising(clk), Timer::new(ns(1))) }, label)
                .await;
            p.read(&[clk, count]);
        },
    );
}

#[test]
#[expect(clippy::many_single_char_names, reason = "the DUT's own net names")]
fn comb_settle() {
    let mut m = FakeModel::new(Precision::PS);
    let a = m.signal("a", 8);
    let b = m.signal("b", 8);
    let y = m.signal("y", 8);
    let y_eq = m.signal("y_eq", 1);
    let s1 = m.signal("s1", 8);
    let s2 = m.signal("s2", 8);
    let s3 = m.signal("s3", 8);
    m.comb(move |s| {
        s.set(s1, s.get(a) + 1);
        s.set(s2, s.get(s1) ^ 0x5a);
        let v = s.get(s2);
        s.set(s3, (v << 1 | v >> 7) & 0xff);
        s.set(y, s.get(s3) + s.get(b));
        s.set(y_eq, u64::from(s.get(y) == s.get(b)));
    });
    let sim = Sim::from_fake(m);
    let sig = |n| sim.signal(n).unwrap();
    let [a, b, y, y_eq, s1, s2, s3] = ["a", "b", "y", "y_eq", "s1", "s2", "s3"].map(sig);
    let log = Log::default();
    let mut p = log.probe("main");
    let mut w = log.probe("y_watch");
    check(
        "comb_settle",
        include_str!("golden/comb_settle.jsonl"),
        sim,
        &log,
        async move {
            p.write(a, 0);
            p.write(b, 0);
            p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
            p.read(&[a, b, y, y_eq]);
            let watcher = spawn("y_watch", async move {
                for _ in 0..3 {
                    w.wait(changed(y), "ValueChange(y)").await;
                    w.read(&[a, y]);
                }
            });
            for (va, vb) in [(0x10, 0), (0x20, 3), (0xff, 0x40)] {
                p.write(a, va);
                p.write(b, vb);
                p.read(&[a, y]);
                p.wait(ReadWrite::new(), "ReadWrite").await;
                p.read(&[a, y]);
                p.wait(ReadOnly::new(), "ReadOnly").await;
                p.read(&[a, b, y, y_eq, s1, s2, s3]);
                p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
                p.read(&[a, y]);
            }
            p.wait(ReadWrite::new(), "ReadWrite").await;
            p.write(a, 0x01);
            p.read(&[a, y]);
            p.wait(ReadOnly::new(), "ReadOnly").await;
            p.read(&[a, y]);
            watcher.await;
        },
    );
}

#[test]
fn derived_edge() {
    let mut m = FakeModel::new(Precision::PS);
    let clk = m.signal("clk", 1);
    let rst = m.signal("rst", 1);
    let div = m.signal("div_clk", 1);
    let fast = m.signal("fast_count", 8);
    let slow = m.signal("slow_count", 8);
    m.on_rising(clk, move |s| {
        if s.get(rst) == 1 {
            s.set(div, 0);
            s.set(fast, 0);
        } else {
            s.set(div, s.get(div) ^ 1);
            s.set(fast, s.get(fast) + 1);
        }
    });
    // The DUT resets slow_count asynchronously; rst is high from time 0, while it is 0.
    m.on_rising(div, move |s| {
        let v = if s.get(rst) == 1 { 0 } else { s.get(slow) + 1 };
        s.set(slow, v);
    });
    let (sim, clk) = sim_with_clock(m);
    let sig = |n| sim.signal(n).unwrap();
    let [rst, div, fast, slow] = ["rst", "div_clk", "fast_count", "slow_count"].map(sig);
    let log = Log::default();
    let mut p = log.probe("main");
    let mut f = log.probe("fast");
    check(
        "derived_edge",
        include_str!("golden/derived_edge.jsonl"),
        sim,
        &log,
        async move {
            clk.write_u64(0);
            p.write(rst, 1);
            p.wait(cycles(clk, 2), "ClockCycles(clk,2)").await;
            p.wait(falling(clk), "FallingEdge(clk)").await;
            p.write(rst, 0);
            let t = spawn("fast", async move {
                for _ in 0..6 {
                    f.wait(rising(clk), "RisingEdge(clk)").await;
                    f.read(&[clk, div, fast, slow]);
                }
            });
            for _ in 0..3 {
                p.wait(rising(div), "RisingEdge(div_clk)").await;
                p.read(&[clk, div, fast, slow]);
                p.wait(ReadOnly::new(), "ReadOnly").await;
                p.read(&[div, fast, slow]);
            }
            t.await;
        },
    );
}
