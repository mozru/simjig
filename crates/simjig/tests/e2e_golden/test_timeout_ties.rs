//! `tests/conformance/tb/test_timeout_ties.py`.

use std::future::Future;

use simjig::{SimDuration, Timer, cycles, ns, ps, spawn, with_timeout};

use crate::conflog::{Log, Probe};
use crate::flop::Dut;
use crate::start_clock;

/// `wait_5ns()`.
async fn wait_5ns() {
    Timer::new(ns(5)).await;
}

/// `outcome()`: awaits `with_timeout(timeout, future)` and logs its outcome and the state.
async fn outcome<T>(
    p: &mut Probe,
    dut: Dut,
    label: &str,
    future: impl Future<Output = T>,
    timeout: SimDuration,
) {
    let result = with_timeout(timeout, future).await;
    p.woke(label);
    let outcome = if result.is_ok() {
        "fired"
    } else {
        "SimTimeoutError"
    };
    p.note("outcome", outcome);
    p.read(&[&dut.clk, &dut.q]);
}

#[simjig::test]
async fn timeout_ties(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    dut.clk.set(0);
    start_clock(dut.clk, 10);
    p.write(&dut.rst, 1);
    p.write(&dut.d, 0);
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);
    let label = "with_timeout(Timer(5ns),5ns)";
    outcome(&mut p, dut, label, Timer::new(ns(5)), ns(5)).await;
    let label = "with_timeout(wait_5ns(),5ns)";
    outcome(&mut p, dut, label, wait_5ns(), ns(5)).await;
    let task = spawn("task", wait_5ns());
    let label = "with_timeout(Task(wait_5ns()),5ns)";
    outcome(&mut p, dut, label, task, ns(5)).await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    let label = "with_timeout(RisingEdge(clk),5ns)";
    outcome(&mut p, dut, label, dut.clk.rising(), ns(5)).await;
    let label = "with_timeout(ClockCycles(clk,2),10ns)";
    outcome(&mut p, dut, label, cycles(dut.clk, 2), ns(10)).await;
    let label = "with_timeout(Timer(4999ps),5ns)";
    outcome(&mut p, dut, label, Timer::new(ps(4999)), ns(5)).await;
    let task = spawn("task", wait_5ns());
    p.wait(Timer::new(ps(1)), "Timer(1step)").await;
    let label = "with_timeout(Task(wait_5ns()),4999ps)";
    outcome(&mut p, dut, label, task, ps(4999)).await;
    log.check("timeout_ties");
}
