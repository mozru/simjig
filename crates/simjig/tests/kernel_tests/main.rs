//! Kernel integration tests, all against `FakeModel` (no Verilator needed).
#![expect(clippy::unwrap_used, reason = "test helpers outside #[test] functions")]

mod bench;
mod cancel;
mod clocks;
mod conflog;
mod conformance;
mod failures;
mod multi;
mod phases;
mod regions;
mod swarm;
mod sync;
mod timing;
mod unrun_after_error;
mod unrun_main_exemption;
mod with_timeout_fn;

use simjig::{Clock, FakeModel, Precision, RawSig, Sim, SimDuration};

/// A small design: `clk`, inputs `a` and `b` (8 bits), `sum = a + b` (combinational) and
/// `q`, a flop of `a` on `clk`.
pub(crate) struct Dut {
    pub(crate) sim: Sim,
    pub(crate) clk: RawSig,
    pub(crate) a: RawSig,
    pub(crate) b: RawSig,
    pub(crate) sum: RawSig,
    pub(crate) q: RawSig,
}

pub(crate) fn dut() -> Dut {
    let mut m = FakeModel::new(Precision::NS);
    let clk = m.signal("clk", 1);
    let a = m.signal("a", 8);
    let b = m.signal("b", 8);
    let sum = m.signal("sum", 8);
    let q = m.signal("q", 8);
    m.comb(move |s| s.set(sum, s.get(a) + s.get(b)));
    m.on_rising(clk, move |s| s.set(q, s.get(a)));
    let sim = Sim::from_fake(m);
    let sig = |n| sim.signal(n).unwrap();
    Dut {
        clk: sig("clk"),
        a: sig("a"),
        b: sig("b"),
        sum: sig("sum"),
        q: sig("q"),
        sim,
    }
}

/// [`dut`] with a 10-tick clock on `clk`, rising at 0, 10, 20, ...
pub(crate) fn clocked() -> Dut {
    let mut d = dut();
    d.sim
        .add_clock(Clock::new(d.clk, SimDuration::from_ticks(10)))
        .unwrap();
    d
}

pub(crate) const fn ticks(n: u64) -> SimDuration {
    SimDuration::from_ticks(n)
}

/// Asserts that every kernel table is empty after a run.
#[track_caller]
pub(crate) fn assert_clean(sim: &Sim) {
    let s = sim.stats();
    assert_eq!(
        (s.live_tasks, s.live_waiters, s.live_wait_lists),
        (0, 0, 0),
        "tables not empty after the run: {s:?}"
    );
}
