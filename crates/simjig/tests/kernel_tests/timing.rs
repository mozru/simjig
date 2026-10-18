//! `--timing` models (`FakeModel` timed events, as strict as Verilator 5.052) interleaved
//! with kernel clocks and Timers.

use std::collections::BTreeSet;

use simjig::{
    Clock, DELTA_MAX, FailureKind, FakeModel, FakeSig, FakeState, Precision, ReadOnly, Sim,
    SimTime, Timer, TraceKind, changed, now, rising,
};

use super::ticks;

/// `always #half sig = ~sig;` with its first toggle at `half`.
fn oscillate(sig: FakeSig, half: u64, s: &mut FakeState) {
    s.set(sig, s.get(sig) ^ 1);
    let next = SimTime(s.now().ticks() + half);
    s.schedule(next, move |s| oscillate(sig, half, s));
}

/// A design with its own `#half` clock `iclk` and a counter of its rising edges, plus a
/// testbench clock input `clk`.
fn timed_design(half: u64) -> FakeModel {
    let mut m = FakeModel::new(Precision::NS);
    m.signal("clk", 1);
    let iclk = m.signal("iclk", 1);
    let ticks_seen = m.signal("ticks", 16);
    m.on_rising(iclk, move |s| s.set(ticks_seen, s.get(ticks_seen) + 1));
    m.at(SimTime(half), move |s| oscillate(iclk, half, s));
    m
}

fn steps(sim: &Sim) -> Vec<u64> {
    sim.recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect()
}

fn multiples(of: u64, upto: u64) -> impl Iterator<Item = u64> {
    (0..=upto / of).map(move |i| i * of)
}

#[test]
fn a_timer_resumes_before_the_designs_event_at_the_same_time() {
    // A `#10 x = 1` in the design and a Timer due at 10: the Timer task sees x = 0 (as
    // cocotb on Verilator: timed callbacks run before the eval that runs the event).
    let mut m = FakeModel::new(Precision::NS);
    let x = m.signal("x", 1);
    m.at(SimTime(10), move |s| s.set(x, 1));
    let mut sim = Sim::from_fake(m);
    let x = sim.signal("x").unwrap();
    let seen = sim.run(async move {
        Timer::new(ticks(10)).await;
        let at_timer = x.read_u64();
        ReadOnly::new().await;
        (at_timer, x.read_u64())
    });
    assert_eq!(seen.unwrap(), (0, 1));
}

#[test]
fn design_slots_kernel_clock_and_timers_interleave_without_a_missed_slot() {
    // The spike's merged loop: design slots every 5 ns, testbench edges every 7 ns.
    let mut sim = Sim::from_fake(timed_design(5));
    let clk = sim.signal("clk").unwrap();
    let counted = sim.signal("ticks").unwrap();
    sim.add_clock(Clock::new(clk, ticks(14))).unwrap();
    let seen = sim
        .run(async move {
            Timer::new(ticks(33)).await;
            Timer::new(ticks(67)).await; // 100
            (now(), counted.read_u64())
        })
        .unwrap();
    // iclk rises at 5, 15, ..., 95: 10 edges (the strict fake fails any skipped slot).
    assert_eq!(seen, (SimTime(100), 10));
    let expect: BTreeSet<u64> = multiples(5, 100)
        .chain(multiples(7, 100))
        .chain([33])
        .collect();
    let steps = steps(&sim);
    assert!(steps.windows(2).all(|w| w[0] < w[1]), "{steps:?}");
    assert_eq!(steps, expect.into_iter().collect::<Vec<_>>());
}

#[test]
fn two_timed_models_and_a_kernel_clock_share_one_timeline() {
    let mut sim = Sim::from_fakes([timed_design(5), timed_design(3)]).unwrap();
    let clk = sim.signal_in(1, "clk").unwrap();
    let (n0, n1) = (
        sim.signal("ticks").unwrap(),
        sim.signal_in(1, "ticks").unwrap(),
    );
    sim.add_clock(Clock::new(clk, ticks(8))).unwrap();
    let seen = sim
        .run(async move {
            Timer::new(ticks(31)).await;
            (n0.read_u64(), n1.read_u64())
        })
        .unwrap();
    // Model 0 rises at 5, 15, 25; model 1 at 3, 9, 15, 21, 27.
    assert_eq!(seen, (3, 5));
    let expect: BTreeSet<u64> = multiples(5, 30)
        .chain(multiples(3, 30))
        .chain(multiples(4, 30))
        .chain([31])
        .collect();
    assert_eq!(steps(&sim), expect.into_iter().collect::<Vec<_>>());
}

#[test]
fn a_model_whose_events_run_out_is_no_longer_asked_for_its_next_slot() {
    // The fake panics if next_time_slot is called with nothing pending, as Verilator
    // exits; the clock keeps time going after the model's only event.
    let mut m = FakeModel::new(Precision::NS);
    m.signal("clk", 1);
    let x = m.signal("x", 1);
    m.at(SimTime(3), move |s| s.set(x, 1));
    let mut sim = Sim::from_fake(m);
    let clk = sim.signal("clk").unwrap();
    let x = sim.signal("x").unwrap();
    sim.add_clock(Clock::new(clk, ticks(10))).unwrap();
    let t = sim
        .run(async move {
            changed(x).await;
            for _ in 0..3 {
                rising(clk).await;
            }
            now()
        })
        .unwrap();
    assert_eq!(t, SimTime(30));
}

#[test]
fn a_zero_delay_event_is_another_delta_of_the_same_timestep() {
    let mut m = FakeModel::new(Precision::NS);
    let x = m.signal("x", 8);
    m.at(SimTime(10), move |s| {
        s.set(x, 1);
        s.schedule(SimTime(10), move |s| s.set(x, 2)); // #0
    });
    let mut sim = Sim::from_fake(m);
    let x = sim.signal("x").unwrap();
    let seen = sim
        .run(async move {
            changed(x).await;
            let first = (now(), x.read_u64());
            changed(x).await;
            (first, now(), x.read_u64())
        })
        .unwrap();
    assert_eq!(seen, ((SimTime(10), 1), SimTime(10), 2));
    assert_eq!(steps(&sim), [0, 10]);
}

#[test]
fn a_zero_delay_loop_hits_the_delta_limit() {
    fn again(x: FakeSig, s: &mut FakeState) {
        s.set(x, s.get(x) ^ 1);
        s.schedule(s.now(), move |s| again(x, s));
    }
    let mut m = FakeModel::new(Precision::NS);
    let x = m.signal("x", 1);
    m.at(SimTime(4), move |s| again(x, s));
    let mut sim = Sim::from_fake(m);
    let f = sim.run(Timer::new(ticks(10))).unwrap_err();
    assert_eq!(f.time, SimTime(4));
    assert!(
        matches!(f.kind, FailureKind::DeltaLimit { limit, .. } if limit == DELTA_MAX),
        "{f}"
    );
}

#[test]
fn an_event_scheduled_in_the_past_is_a_model_failure() {
    let mut m = FakeModel::new(Precision::NS);
    let x = m.signal("x", 1);
    m.at(SimTime(5), move |s| {
        s.schedule(SimTime(2), move |s| s.set(x, 1));
    });
    let mut sim = Sim::from_fake(m);
    let f = sim.run(Timer::new(ticks(10))).unwrap_err();
    assert_eq!(f.time, SimTime(5));
    let FailureKind::ModelFatal { message } = &f.kind else {
        panic!("{f}")
    };
    assert!(
        message.contains("next event at t=2 ns, before now"),
        "{message}"
    );
}

/// `now_display()` and `simjig::log!`'s prefix print the time in the design's units and
/// the running task, with no precision passed in.
#[test]
fn log_prefix_names_the_time_in_units_and_the_task() {
    let mut sim = Sim::from_fake(FakeModel::new(Precision::PS));
    let (shown, prefix, main_prefix) = sim
        .run(async {
            let task = simjig::spawn("monitor", async {
                Timer::new(simjig::ns(1)).await;
                Timer::new(simjig::SimDuration::from_ticks(250)).await;
                simjig::log!("from the task");
                (
                    simjig::now_display().to_string(),
                    simjig::__private::log_prefix(),
                )
            });
            let (shown, prefix) = task.await;
            (shown, prefix, simjig::__private::log_prefix())
        })
        .unwrap();
    assert_eq!(shown, "1250 ps");
    assert_eq!(prefix, "[1250 ps main.monitor]");
    assert_eq!(main_prefix, "[1250 ps main]");
}
