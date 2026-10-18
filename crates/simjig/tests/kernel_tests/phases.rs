//! Scheduler-loop and trigger rules not already pinned by the conformance goldens.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use simjig::{
    Clock, FailureKind, FakeModel, NextTimeStep, Precision, ReadOnly, ReadWrite, Sim, SimTime,
    Timer, changed, falling, now, rising, spawn, yield_now,
};

use super::{assert_clean, clocked, dut, ticks};

type Trace = Rc<RefCell<Vec<String>>>;

fn push(trace: &Trace, s: impl Into<String>) {
    trace.borrow_mut().push(s.into());
}

#[test]
fn writes_are_deposits_visible_only_after_application() {
    let mut d = dut();
    let (a, b, sum) = (d.a, d.b, d.sum);
    let seen = d.sim.run(async move {
        a.write_u64(3);
        b.write_u64(4);
        let before = (a.read_u64(), sum.read_u64());
        ReadWrite::new().await;
        (before, a.read_u64(), sum.read_u64())
    });
    assert_eq!(seen.unwrap(), ((0, 0), 3, 7));
    assert_clean(&d.sim);
}

#[test]
fn edge_waiters_wake_in_registration_order_across_signals() {
    // Two clocks with coincident edges at 0, 20, 40...: the waiter armed first wakes first,
    // whatever the clock declaration order (golden two_clocks, rule 5).
    let mut m = FakeModel::new(Precision::NS);
    m.signal("c1", 1);
    m.signal("c2", 1);
    let mut sim = Sim::from_fake(m);
    let c1 = sim.signal("c1").unwrap();
    let c2 = sim.signal("c2").unwrap();
    sim.add_clock(Clock::new(c1, ticks(10))).unwrap();
    sim.add_clock(Clock::new(c2, ticks(20))).unwrap();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    sim.run(async move {
        Timer::new(ticks(1)).await;
        let t2 = Rc::clone(&t);
        let on_c2 = spawn("on_c2", async move {
            rising(c2).await;
            push(
                &t2,
                format!("c2 at {}, c1={}", now().ticks(), c1.read_u64()),
            );
        });
        yield_now().await; // let on_c2 arm first
        rising(c1).await; // 10: c1 only
        rising(c1).await; // 20: coincident with c2, armed after on_c2
        push(&t, format!("c1 at {}, c2={}", now().ticks(), c2.read_u64()));
        on_c2.await;
    })
    .unwrap();
    // Both clocks were applied before either edge fired.
    assert_eq!(*trace.borrow(), ["c2 at 20, c1=1", "c1 at 20, c2=1"]);
}

#[test]
fn read_write_inside_read_write_waits_for_the_next_region() {
    let mut d = clocked();
    let a = d.a;
    let times = d.sim.run(async move {
        Timer::new(ticks(3)).await;
        ReadWrite::new().await;
        let first = now();
        // Nothing written: this region ends, and the next ReadWrite region is at the
        // next timestep (the falling clock edge at 5).
        ReadWrite::new().await;
        let second = now();
        a.write_u64(1);
        ReadWrite::new().await; // the write makes the loop go round: same timestep
        (first, second, now())
    });
    assert_eq!(times.unwrap(), (SimTime(3), SimTime(5), SimTime(5)));
}

#[test]
fn next_time_step_resumes_at_the_next_scheduled_time_before_timers() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let t2 = Rc::clone(&t);
            let timer = spawn("timer", async move {
                Timer::new(ticks(7)).await;
                push(&t2, format!("timer {}", now().ticks()));
            });
            NextTimeStep::new().await;
            push(&t, format!("next {}", now().ticks()));
            timer.await;
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["next 7", "timer 7"]);
}

#[test]
fn yield_now_lets_ready_tasks_run_first() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let t2 = Rc::clone(&t);
            let child = spawn("child", async move { push(&t2, "child") });
            push(&t, "main before");
            yield_now().await;
            push(&t, "main after");
            child.await;
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["main before", "child", "main after"]);
}

#[test]
fn falling_and_changed_follow_value_changes() {
    let mut d = clocked();
    let (clk, a, q) = (d.clk, d.a, d.q);
    let seen = d.sim.run(async move {
        falling(clk).await;
        let fall = now();
        a.write_u64(9);
        changed(q).await; // the flop takes a=9 at the rising edge at 10
        (fall, now(), q.read_u64())
    });
    assert_eq!(seen.unwrap(), (SimTime(5), SimTime(10), 9));
}

#[test]
fn model_events_advance_time() {
    // A `--timing` stand-in: the model has an event at 42 and nothing else is scheduled.
    let mut m = FakeModel::new(Precision::NS);
    let x = m.signal("x", 1);
    m.at(SimTime(42), move |s| s.set(x, 1));
    let mut sim = Sim::from_fake(m);
    let x = sim.signal("x").unwrap();
    let t = sim.run(async move {
        changed(x).await;
        now()
    });
    assert_eq!(t.unwrap(), SimTime(42));
}

#[test]
fn time_is_monotonic_and_steps_are_distinct() {
    let mut d = clocked();
    let clk = d.clk;
    d.sim
        .run(async move {
            for _ in 0..5 {
                rising(clk).await;
                Timer::new(ticks(3)).await;
            }
        })
        .unwrap();
    let steps: Vec<u64> = d
        .sim
        .recent_trace()
        .iter()
        .filter(|r| r.kind == simjig::TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect();
    assert!(steps.windows(2).all(|w| w[0] < w[1]), "{steps:?}");
    assert_eq!(&steps[..4], [0, 3, 5, 10]);
}

#[test]
fn same_program_gives_same_trace_hash() {
    let run = |delay: u64| {
        let mut d = clocked();
        let (clk, a) = (d.clk, d.a);
        d.sim
            .run(async move {
                let child = spawn("w", async move {
                    for i in 0..4 {
                        rising(clk).await;
                        a.write_u64(i);
                    }
                });
                Timer::new(ticks(delay)).await;
                child.await;
            })
            .unwrap();
        d.sim.trace_hash()
    };
    assert_eq!(run(7), run(7));
    assert_ne!(run(7), run(8));
}

#[test]
fn triggers_work_under_a_foreign_waker() {
    // A combinator that polls with its own waker (like FuturesUnordered) must be woken
    // through that waker: triggers store cx.waker(), not the task id.
    struct Counting {
        inner: Waker,
        wakes: AtomicUsize,
    }
    impl Wake for Counting {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::Relaxed);
            self.inner.wake_by_ref();
        }
    }
    let mut d = clocked();
    let clk = d.clk;
    let wakes = d.sim.run(async move {
        let mut edge = std::pin::pin!(rising(clk));
        let mut counting: Option<Arc<Counting>> = None;
        std::future::poll_fn(|cx: &mut Context<'_>| {
            let c = counting.get_or_insert_with(|| {
                Arc::new(Counting {
                    inner: cx.waker().clone(),
                    wakes: AtomicUsize::new(0),
                })
            });
            let waker = Waker::from(Arc::clone(c));
            edge.as_mut().poll(&mut Context::from_waker(&waker))
        })
        .await;
        counting.map(|c| c.wakes.load(Ordering::Relaxed))
    });
    assert_eq!(wakes.unwrap(), Some(1));
}

#[test]
fn waking_from_another_thread_panics() {
    let mut d = dut();
    let outcome = d.sim.run(async move {
        let waker = std::future::poll_fn(|cx| Poll::Ready(cx.waker().clone())).await;
        std::thread::spawn(move || waker.wake()).join().is_err()
    });
    assert!(
        outcome.unwrap(),
        "a cross-thread wake must panic on the waking thread"
    );
}

#[test]
fn read_only_rejects_reentry_and_timer_zero_is_rejected() {
    for case in 0..2 {
        let mut d = dut();
        let failure = d
            .sim
            .run(async move {
                if case == 0 {
                    ReadOnly::new().await;
                    ReadOnly::new().await;
                } else {
                    Timer::new(ticks(0)).await;
                }
            })
            .unwrap_err();
        let FailureKind::TaskPanic { message } = &failure.kind else {
            panic!("{failure}")
        };
        let expect = if case == 0 {
            "ReadOnly phase"
        } else {
            "zero ticks"
        };
        assert!(message.contains(expect), "{message}");
        assert_clean(&d.sim);
    }
}

/// The 1-based line of this file that contains `marker` (once, besides this lookup).
fn line_of(marker: &str) -> u32 {
    let source = include_str!("phases.rs");
    let index = source
        .lines()
        .position(|l| l.contains(marker) && !l.contains("line_of"))
        .unwrap();
    u32::try_from(index + 1).unwrap()
}

#[test]
fn trigger_misuse_points_at_the_line_that_made_the_trigger() {
    type Main = fn() -> std::pin::Pin<Box<dyn Future<Output = ()>>>;
    let cases: [(Main, &str); 3] = [
        (
            || {
                Box::pin(async {
                    ReadOnly::new().await;
                    ReadOnly::new().await; // misuse: read-only twice
                })
            },
            "misuse: read-only twice",
        ),
        (
            || Box::pin(Timer::new(ticks(0))), // misuse: zero timer
            "misuse: zero timer",
        ),
        (
            || {
                Box::pin(async {
                    ReadOnly::new().await;
                    ReadWrite::default().await; // misuse: read-write after read-only
                })
            },
            "misuse: read-write after read-only",
        ),
    ];
    for (main, marker) in cases {
        let mut d = dut();
        let failure = d.sim.run(main()).unwrap_err();
        let location = failure.location.unwrap();
        assert!(location.file().ends_with("phases.rs"), "{failure}");
        assert_eq!(location.line(), line_of(marker), "{failure}");
        let FailureKind::TaskPanic { message } = &failure.kind else {
            panic!("{failure}")
        };
        assert!(
            message.contains("phases.rs"),
            "the message names the line: {message}"
        );
        assert_clean(&d.sim);
    }
}
