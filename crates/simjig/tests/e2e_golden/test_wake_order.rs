//! `tests/conformance/tb/test_wake_order.py`: the order in which one timestep's value
//! changes wake tasks, and when `first!`, `join!` and `with_timeout` resume their caller.

use simjig::{Event, RawSig, Timer, changed, cycles, ns, spawn};

use crate::conflog::{Log, Probe};
use crate::start_clock;

#[simjig::test]
async fn coincident_join(dut: crate::async_fifo::Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let mut pr = log.probe("r_task");
    let mut pw = log.probe("w_task");
    let mut pz = log.probe("z_task");
    let mut pq = log.probe("q_task");
    let ev = Event::new("ev");
    dut.wclk.set(0);
    dut.rclk.set(0);
    start_clock(dut.wclk, 10);
    start_clock(dut.rclk, 30);
    p.write(&dut.winc, 0);
    let seen = move |probe: &Probe| probe.read(&[&dut.wclk, &dut.rclk, &dut.winc]);

    let set = ev.clone();
    let r_task = async move {
        pr.wait(dut.rclk.rising(), "RisingEdge(rclk)").await; // 15 ns
        seen(&pr);
        pr.write(&dut.winc, 1);
        set.set();
        pr.wait(dut.wclk.rising(), "RisingEdge(wclk)").await; // 15 ns, joined
        seen(&pr);
        pr.wait(dut.rclk.rising(), "RisingEdge(rclk)").await; // 45 ns
        seen(&pr);
        pr.wait(dut.wclk.rising(), "RisingEdge(wclk)").await; // 55 ns
        seen(&pr);
        pr.wait(Timer::new(ns(11)), "Timer(11ns)").await; // 66 ns
        seen(&pr);
        pr.wait(dut.rclk.rising(), "RisingEdge(rclk)").await; // 75 ns
        seen(&pr);
        pr.wait(dut.wclk.rising(), "RisingEdge(wclk)").await; // 85 ns
        seen(&pr);
    };
    let w_task = async move {
        for _ in 0..2 {
            pw.wait(dut.wclk.rising(), "RisingEdge(wclk)").await;
            seen(&pw);
        }
    };
    let z_task = async move {
        pz.wait(ev.wait(), "Event.wait()").await;
        seen(&pz);
    };
    let q_task = async move {
        for _ in 0..3 {
            pq.wait(dut.wclk.rising(), "RisingEdge(wclk)").await;
            seen(&pq);
        }
    };
    let tr = spawn("r_task", r_task);
    p.wait(Timer::new(ns(10)), "Timer(10ns)").await;
    let tw = spawn("w_task", w_task);
    let tz = spawn("z_task", z_task);
    p.wait(Timer::new(ns(50)), "Timer(50ns)").await;
    let tq = spawn("q_task", q_task);
    tr.await;
    tw.await;
    tz.await;
    tq.await;
    log.check("coincident_join");
}

/// `join_on()`: `first` and `second` change in one walk, `first`'s group primed first.
/// Each step writes `a` and, if given, `b`, at 2, 4 and 6 ns.
async fn join_on(
    dut: crate::comb_chain::Dut,
    log: &Log,
    (first, first_label): (RawSig, &'static str),
    (second, second_label): (RawSig, &'static str),
    steps: [(u128, Option<u128>); 3],
) {
    let mut p = log.probe("main");
    let mut pa = log.probe("a_task");
    let mut pb = log.probe("b_task");
    let mut pz = log.probe("z_task");
    let ev = Event::new("ev");
    let seen = move |probe: &Probe| probe.read(&[&dut.a, &dut.b, &dut.y, &dut.y_eq]);
    p.write(&dut.a, 0);
    p.write(&dut.b, 0);
    p.wait(Timer::new(ns(1)), "Timer(1ns)").await;

    let set = ev.clone();
    let a_task = async move {
        for _ in 0..2 {
            pa.wait(changed(first), first_label).await;
            seen(&pa);
            set.set();
            pa.wait(changed(second), second_label).await;
            seen(&pa);
        }
    };
    let b_task = async move {
        pb.wait(changed(second), second_label).await;
        seen(&pb);
    };
    let z_task = async move {
        pz.wait(ev.wait(), "Event.wait()").await;
        seen(&pz);
    };
    let ta = spawn("a_task", a_task);
    let tb = spawn("b_task", b_task);
    let tz = spawn("z_task", z_task);
    for (i, (a, b)) in steps.into_iter().enumerate() {
        let delay = if i == 0 { 1 } else { 2 };
        p.wait(Timer::new(ns(delay)), &format!("Timer({delay}ns)"))
            .await;
        p.write(&dut.a, a);
        if let Some(b) = b {
            p.write(&dut.b, b);
        }
    }
    ta.await;
    tb.await;
    tz.await;
}

#[simjig::test]
async fn changed_join(dut: crate::comb_chain::Dut) {
    let log = Log::default();
    let steps = [(0x59, None), (0, None), (0x59, None)];
    join_on(
        dut,
        &log,
        (dut.y.raw(), "ValueChange(y)"),
        (dut.y_eq.raw(), "ValueChange(y_eq)"),
        steps,
    )
    .await;
    log.check("changed_join");
}

#[simjig::test]
async fn written_join(dut: crate::comb_chain::Dut) {
    let log = Log::default();
    let steps = [(1, Some(1)), (2, Some(2)), (3, Some(3))];
    join_on(
        dut,
        &log,
        (dut.a.raw(), "ValueChange(a)"),
        (dut.b.raw(), "ValueChange(b)"),
        steps,
    )
    .await;
    log.check("written_join");
}

/// `counter_start()`.
async fn counter_start(p: &mut Probe, dut: crate::counter::Dut) {
    dut.clk.set(0);
    start_clock(dut.clk, 10);
    p.write(&dut.rst, 1);
    p.write(&dut.en, 1);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);
}

#[simjig::test]
async fn edge_kinds(dut: crate::counter::Dut) {
    let (clk, count) = (dut.clk, dut.count);
    let log = Log::default();
    let mut p = log.probe("main");
    let mut pv = log.probe("vc_task");
    let mut pe = log.probe("edge_task");
    let mut pz = log.probe("z_task");
    let mut pc = log.probe("cycles_task");
    let ev = Event::new("ev");
    counter_start(&mut p, dut).await; // 10 ns
    let seen = move |probe: &Probe| probe.read(&[&clk, &count]);

    let set = ev.clone();
    let vc_task = async move {
        pv.wait(clk.changed(), "ValueChange(clk)").await; // 15 ns
        seen(&pv);
        set.set();
        pv.wait(clk.rising(), "RisingEdge(clk)").await; // 15 ns, joined
        seen(&pv);
        for _ in 0..2 {
            pv.wait(clk.changed(), "ValueChange(clk)").await; // 20, 25 ns
            seen(&pv);
        }
        pv.wait(clk.rising(), "RisingEdge(clk)").await; // 35 ns
        seen(&pv);
        for _ in 0..2 {
            pv.wait(clk.changed(), "ValueChange(clk)").await; // 40, 45 ns
            seen(&pv);
        }
        pv.wait(cycles(clk, 1), "ClockCycles(clk,1)").await; // 45 ns, joined
        seen(&pv);
    };
    let edge_task = async move {
        pe.wait(clk.rising(), "RisingEdge(clk)").await; // 15 ns
        seen(&pe);
    };
    let z_task = async move {
        pz.wait(ev.wait(), "Event.wait()").await;
        seen(&pz);
    };
    let cycles_task = async move {
        pc.wait(cycles(clk, 2), "ClockCycles(clk,2)").await; // 55 ns
        seen(&pc);
    };
    let tv = spawn("vc_task", vc_task);
    let te = spawn("edge_task", edge_task);
    let tz = spawn("z_task", z_task);
    p.wait(Timer::new(ns(31)), "Timer(31ns)").await; // 41 ns
    let tc = spawn("cycles_task", cycles_task);
    tv.await;
    te.await;
    tz.await;
    tc.await;
    log.check("edge_kinds");
}
