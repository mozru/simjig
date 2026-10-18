//! Kernel clocks controlled from inside a run: `clock()`, `ClockHandle` and the handle
//! `Sim::add_clock` returns.

use std::future::Future;

use simjig::{
    Clock, FailureKind, FakeModel, NextTimeStep, Precision, RawSig, ReadOnly, Sim, SimTime, Timer,
    TraceKind, clock, cycles, falling, now, rising, with_timeout,
};

use super::{assert_clean, clocked, dut, ticks};

/// `(time, delta of the eval it followed, slot, value)` of every write the kernel applied.
fn applied_writes(sim: &Sim) -> Vec<(u64, u64, u64, u64)> {
    let mut delta = 0;
    let mut out = Vec::new();
    for r in sim.recent_trace() {
        match r.kind {
            TraceKind::Eval => delta = r.arg,
            TraceKind::Write => out.push((r.time.ticks(), delta, r.arg, r.value)),
            _ => {}
        }
    }
    out
}

/// Waits for `n` rising edges of `sig` and returns their times.
async fn rise_times(sig: RawSig, n: usize) -> Vec<u64> {
    let mut times = Vec::new();
    for _ in 0..n {
        rising(sig).await;
        times.push(now().ticks());
    }
    times
}

#[test]
fn a_clock_started_by_main_at_zero_runs_exactly_like_an_added_one() {
    let body = |clk: RawSig, q: RawSig, a: RawSig| async move {
        cycles(clk, 2).await;
        a.write_u64(7); // captured by the edge at 20
        cycles(clk, 2).await;
        (now(), q.read_u64())
    };
    let mut added = clocked();
    let out_added = added.sim.run(body(added.clk, added.q, added.a)).unwrap();

    let mut started = dut();
    let (clk, q, a) = (started.clk, started.q, started.a);
    let out_started = started
        .sim
        .run(async move {
            clock(clk, ticks(10)).start();
            body(clk, q, a).await
        })
        .unwrap();
    assert_eq!(out_started, out_added);
    assert_eq!(out_started, (SimTime(30), 7));
    assert_eq!(started.sim.trace_hash(), added.sim.trace_hash());
    assert_clean(&started.sim);
}

#[test]
fn start_mid_run_schedules_from_now_with_phase_and_high_time() {
    let mut d = dut();
    let clk = d.clk;
    let (rises, fall) = d
        .sim
        .run(async move {
            Timer::new(ticks(7)).await;
            clock(clk, ticks(10))
                .phase(ticks(2))
                .high_for(ticks(3))
                .start();
            let rises = rise_times(clk, 2).await;
            falling(clk).await;
            (rises, now().ticks())
        })
        .unwrap();
    assert_eq!(rises, [9, 19]);
    assert_eq!(fall, 22);
}

#[test]
fn stop_freezes_the_level_and_start_resumes_from_the_restart_time() {
    let mut d = dut();
    let clk = d.clk;
    let out = d
        .sim
        .run(async move {
            let c = clock(clk, ticks(10)).start();
            rise_times(clk, 2).await; // 0 and 10
            c.stop();
            assert!(!c.is_running());
            let level = clk.read_u64();
            // The falling edge due at 15 is cancelled: nothing changes for 30 ticks.
            let quiet = with_timeout(ticks(30), simjig::changed(clk)).await.is_err();
            let held = clk.read_u64();
            Timer::new(ticks(3)).await; // now 43
            c.start();
            assert!(c.is_running());
            // Started high-phase while already high: the next change is the fall at 48.
            falling(clk).await;
            let fall = now().ticks();
            (level, quiet, held, fall, rise_times(clk, 1).await)
        })
        .unwrap();
    assert_eq!(out, (1, true, 1, 48, vec![53]));
    assert_clean(&d.sim);
}

#[test]
fn an_added_clock_can_be_stopped_from_the_run() {
    let mut d = dut();
    let clk = d.clk;
    let handle = d.sim.add_clock(Clock::new(clk, ticks(10))).unwrap();
    let f = d
        .sim
        .run(async move {
            rising(clk).await;
            assert_eq!(handle.signal(), clk);
            handle.stop();
            rising(clk).await;
        })
        .unwrap_err();
    // Stopped at 0, before its fall at 5: nothing can ever raise clk again.
    assert!(matches!(f.kind, FailureKind::Starved { .. }), "{f}");
    assert_eq!(f.time, SimTime(0));
}

#[test]
fn coincident_edges_of_started_and_added_clocks_share_one_eval_in_declaration_order() {
    let mut m = FakeModel::new(Precision::NS);
    m.signal("c1", 1);
    m.signal("c2", 1);
    m.signal("c3", 1);
    let mut sim = Sim::from_fake(m);
    let [c1, c2, c3] = ["c1", "c2", "c3"].map(|n| sim.signal(n).unwrap());
    sim.add_clock(Clock::new(c2, ticks(10))).unwrap();
    sim.run(async move {
        // Declared c3 then c1; c2 was added before the run.
        let k3 = clock(c3, ticks(20)).start();
        let k1 = clock(c1, ticks(10)).start();
        Timer::new(ticks(25)).await;
        // c3 stops before its fall at 30, so at 30 only the period-10 clocks have edges.
        k3.stop();
        NextTimeStep::new().await; // 30
        Timer::new(ticks(10)).await; // 40, timed region: the restart joins the first delta
        k3.start();
        Timer::new(ticks(20)).await; // 60
        k1.stop();
    })
    .unwrap();
    let at = |t| -> Vec<(u64, u64)> {
        applied_writes(&sim)
            .into_iter()
            .filter(|w| w.0 == t)
            .map(|w| (w.1, w.2))
            .collect()
    };
    // Slots: c1 = 0, c2 = 1, c3 = 2. At 0, the starts were logged by main before the
    // timed region logged c2's edge.
    assert_eq!(at(0), [(0, 2), (0, 0), (0, 1)]);
    // Later edges: one apply after eval 0, in declaration order c2, c3, c1.
    assert_eq!(at(20), [(0, 1), (0, 2), (0, 0)]);
    assert_eq!(at(30), [(0, 1), (0, 0)]);
    // The restart at 40 is logged when called, after the timed region's edges...
    assert_eq!(at(40), [(0, 1), (0, 0), (0, 2)]);
    // ...and the restarted clock keeps its place among later coincident edges.
    assert_eq!(at(50), [(0, 1), (0, 2), (0, 0)]);
}

#[test]
fn misuse_panics_at_the_call() {
    type Case = fn(RawSig, RawSig) -> Box<dyn Future<Output = ()>>;
    let cases: [(Case, &str); 6] = [
        (
            |clk, _| {
                Box::new(async move {
                    clock(clk, ticks(10)).start().start();
                })
            },
            "already running",
        ),
        (
            |clk, _| {
                Box::new(async move {
                    clock(clk, ticks(10)).start().phase(ticks(1));
                })
            },
            "phase() on the running clock of `clk`",
        ),
        (
            |_, a| {
                Box::new(async move {
                    clock(a, ticks(10));
                })
            },
            "cannot clock `a`: it is 8 bits wide",
        ),
        (
            |clk, _| {
                Box::new(async move {
                    clock(clk, ticks(7)).start();
                })
            },
            "a 7-tick period cannot be split 50/50",
        ),
        (
            |clk, _| {
                Box::new(async move {
                    clock(clk, ticks(10)).start();
                    clock(clk, ticks(20));
                })
            },
            "`clk` already has a running clock",
        ),
        (
            |clk, _| {
                Box::new(async move {
                    ReadOnly::new().await;
                    clock(clk, ticks(10)).start();
                })
            },
            "started in the ReadOnly phase",
        ),
    ];
    for (case, expect) in cases {
        let mut d = dut();
        let body = Box::into_pin(case(d.clk, d.a));
        let f = d.sim.run(body).unwrap_err();
        let FailureKind::TaskPanic { message } = &f.kind else {
            panic!("{f}")
        };
        assert!(message.contains(expect), "{message}");
        let at = f.location.unwrap();
        assert!(at.file().ends_with("clocks.rs"), "{at}");
        assert_clean(&d.sim);
    }
}

#[test]
fn a_stopped_clock_is_redeclared_with_a_new_period() {
    let mut d = dut();
    let clk = d.clk;
    let rises = d
        .sim
        .run(async move {
            let first = clock(clk, ticks(10)).start();
            rising(clk).await;
            falling(clk).await; // 5
            first.stop();
            let second = clock(clk, ticks(4)).start(); // same entry, new period
            assert_eq!(second, first);
            rise_times(clk, 2).await
        })
        .unwrap();
    assert_eq!(rises, [5, 9]);
}

#[test]
fn a_handle_outside_its_run_panics() {
    let mut d = dut();
    let clk = d.clk;
    let handle = d.sim.run(async move { clock(clk, ticks(10)) }).unwrap();
    let outside = std::panic::catch_unwind(|| handle.start()).unwrap_err();
    let message = outside.downcast_ref::<&str>().copied().unwrap_or_default();
    assert!(
        message.contains("outside the running simulation"),
        "{message}"
    );
}
