//! Every failure kind: when it is raised, and what it says.

use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use simjig::{
    BlockedTask, DELTA_MAX, Event, FailureKind, FakeModel, Lock, Precision, READ_WRITE_MAX,
    ReadOnly, ReadWrite, Sim, SimTime, Timer, changed, rising, spawn,
};

use super::{assert_clean, clocked, dut, ticks};

#[test]
fn write_conflict_names_both_tasks_only_when_values_differ() {
    for (second, conflict) in [(5, false), (6, true)] {
        let mut d = dut();
        let a = d.a;
        let out = d.sim.run(async move {
            a.write_u64(5);
            spawn("other", async move { a.write_u64(second) }).await;
            Timer::new(ticks(1)).await;
            a.read_u64()
        });
        if conflict {
            let f = out.unwrap_err();
            assert_eq!(
                f.kind,
                FailureKind::WriteConflict {
                    signal: "a".into(),
                    first: "task `main`".into(),
                    second: "task `main.other`".into(),
                }
            );
            assert_eq!(f.task.as_deref(), Some("main.other"));
            assert!(f.location.unwrap().file().ends_with("failures.rs"), "{f}");
        } else {
            assert_eq!(out.unwrap(), 5);
        }
        assert_clean(&d.sim);
    }
}

#[test]
fn one_task_rewriting_a_signal_keeps_the_last_value() {
    let mut d = dut();
    let a = d.a;
    let out = d.sim.run(async move {
        a.write_u64(1);
        a.write_u64(2);
        Timer::new(ticks(1)).await;
        a.read_u64()
    });
    assert_eq!(out.unwrap(), 2);
}

#[test]
fn a_task_woken_from_read_only_runs_there_and_cannot_write() {
    // An Event set by a ReadOnly waiter wakes its waiter in that ReadOnly phase; cocotb
    // raises RuntimeError on its write, simjig fails the run.
    let mut d = dut();
    let a = d.a;
    let f = d
        .sim
        .run(async move {
            let event = Rc::new(Event::new("e"));
            let e = Rc::clone(&event);
            let writer = spawn("writer", async move {
                e.wait().await;
                a.write_u64(1);
            });
            ReadOnly::new().await;
            event.set();
            writer.await;
        })
        .unwrap_err();
    assert_eq!(f.kind, FailureKind::WriteInReadOnly { signal: "a".into() });
    assert!(
        f.task.as_deref().is_some_and(|t| t.ends_with("writer")),
        "{f}"
    );
    assert_clean(&d.sim);
}

#[test]
fn write_in_read_only_fails_at_the_writing_line() {
    let mut d = dut();
    let a = d.a;
    let f = d
        .sim
        .run(async move {
            ReadOnly::new().await;
            a.write_u64(1);
        })
        .unwrap_err();
    assert_eq!(f.kind, FailureKind::WriteInReadOnly { signal: "a".into() });
    assert_eq!(f.task.as_deref(), Some("main"));
    assert!(f.location.is_some());
    assert_clean(&d.sim);
}

#[test]
fn delta_limit_names_the_signal_still_written() {
    let mut d = dut();
    let (a, b) = (d.a, d.b);
    let f = d
        .sim
        .run(async move {
            // A testbench "oscillator": every change of a writes a again.
            loop {
                a.write_u64(a.read_u64() ^ 1);
                changed(a).await;
                b.write_u64(0);
            }
        })
        .unwrap_err();
    let FailureKind::DeltaLimit { limit, signals } = &f.kind else {
        panic!("{f}")
    };
    assert_eq!(*limit, DELTA_MAX);
    assert!(signals.contains(&"a".to_owned()), "{f}");
    assert_eq!(f.time, SimTime(0));
    assert_clean(&d.sim);
}

#[test]
fn read_write_loop_backs_have_their_own_higher_limit() {
    // 100 write-then-ReadWrite rounds in one timestep: more than DELTA_MAX evals, but each
    // round settles, so it is not an oscillation.
    let mut d = dut();
    let (a, sum) = (d.a, d.sum);
    let out = d.sim.run(async move {
        for i in 0..100 {
            a.write_u64(i);
            ReadWrite::new().await;
            assert_eq!(sum.read_u64(), i, "the fan-out settled before ReadWrite");
        }
        simjig::now()
    });
    assert_eq!(out.unwrap(), SimTime(0));
    assert_clean(&d.sim);
}

#[test]
fn read_write_limit_names_the_task_and_line_that_keep_looping_back() {
    let mut d = dut();
    let a = d.a;
    let f = d
        .sim
        .run(async move {
            loop {
                a.write_u64(a.read_u64() ^ 1);
                ReadWrite::new().await; // loops back forever
            }
        })
        .unwrap_err();
    let FailureKind::ReadWriteLimit { limit, waiters } = &f.kind else {
        panic!("{f}")
    };
    assert_eq!(*limit, READ_WRITE_MAX);
    assert_eq!(waiters.len(), 1, "{f}");
    assert_eq!(waiters[0].task, "main");
    let line = include_str!("failures.rs")
        .lines()
        .position(|l| l.ends_with("// loops back forever"))
        .unwrap()
        + 1;
    let at = format!("failures.rs:{line}:");
    assert!(waiters[0].awaiting[0].contains(&at), "{f}");
    assert_eq!(f.task.as_deref(), Some("main"));
    assert_eq!(f.location.unwrap().line() as usize, line, "{f}");
    let text = f.to_string();
    assert!(text.contains("task `main` awaits ReadWrite at"), "{text}");
    assert!(!text.contains("signals"), "no empty signal list: {text}");
    assert_eq!(f.time, SimTime(0));
    assert_clean(&d.sim);
}

#[test]
fn starved_lists_what_each_task_awaits() {
    let mut d = dut();
    let (clk, sum) = (d.clk, d.sum);
    let f = d
        .sim
        .run(async move {
            let _child = spawn("waiter", async move { changed(sum).await });
            rising(clk).await;
        })
        .unwrap_err();
    assert_eq!(
        f.kind,
        FailureKind::Starved {
            blocked: vec![
                BlockedTask {
                    task: "main".into(),
                    awaiting: vec!["rising(`clk`)".into()],
                },
                BlockedTask {
                    task: "main.waiter".into(),
                    awaiting: vec!["changed(`sum`)".into()],
                },
            ]
        }
    );
    let text = f.to_string();
    assert!(text.contains("task `main` awaits rising(`clk`)"), "{text}");
    assert!(text.contains("help: start a clock"), "{text}");
    assert_clean(&d.sim);
}

#[test]
fn deadlock_on_crossed_locks() {
    let mut d = dut();
    let f = d
        .sim
        .run(async move {
            let (x, y) = (Lock::new("x"), Lock::new("y"));
            let (x2, y2) = (x.clone(), y.clone());
            let other = spawn("other", async move {
                let _gy = y2.lock().await;
                Timer::new(ticks(1)).await;
                let _gx = x2.lock().await;
            });
            let _gx = x.lock().await;
            Timer::new(ticks(1)).await;
            let _gy = y.lock().await;
            other.await;
        })
        .unwrap_err();
    let FailureKind::Deadlock { blocked } = &f.kind else {
        panic!("{f}")
    };
    let awaits: Vec<_> = blocked
        .iter()
        .map(|b| (b.task.as_str(), b.awaiting.clone()))
        .collect();
    assert_eq!(
        awaits,
        [
            ("main", vec!["Lock `y`".to_owned()]),
            ("main.other", vec!["Lock `x`".to_owned()]),
        ]
    );
    assert_clean(&d.sim);
}

#[test]
fn awaiting_an_event_never_set_is_a_deadlock() {
    let mut d = dut();
    let f = d
        .sim
        .run(async move {
            let e = Event::new("done");
            e.wait().await;
        })
        .unwrap_err();
    assert!(matches!(f.kind, FailureKind::Deadlock { .. }), "{f}");
    assert!(f.to_string().contains("awaits Event `done`"), "{f}");
}

const BAD_BEAT_LINE: u32 = line!() + 2;
fn bad_beat() {
    panic!("bad beat");
}

#[test]
fn task_panic_carries_the_hierarchical_name_and_stops_the_run() {
    let mut d = clocked();
    let clk = d.clk;
    let f = d
        .sim
        .run(async move {
            let inner = spawn("env", async move {
                spawn("driver", async move {
                    rising(clk).await;
                    rising(clk).await;
                    bad_beat();
                })
                .await;
            });
            inner.await;
        })
        .unwrap_err();
    assert_eq!(
        f.kind,
        FailureKind::TaskPanic {
            message: "bad beat".into()
        }
    );
    assert_eq!(f.task.as_deref(), Some("main.env.driver"));
    assert_eq!(f.time, SimTime(10));
    let at = f.location.unwrap();
    assert!(at.file().ends_with("failures.rs"), "{at}");
    assert_eq!((at.line(), at.column()), (BAD_BEAT_LINE, 5));
    assert!(
        f.to_string()
            .starts_with("simjig TaskPanic at t=10 ns in task `main.env.driver`")
    );
    assert_clean(&d.sim);
}

#[test]
fn a_panic_in_cleanup_is_located_too() {
    struct Loud;
    impl Drop for Loud {
        fn drop(&mut self) {
            panic!("cleanup failed");
        }
    }
    let line = line!() - 3;
    let mut d = dut();
    let f = d
        .sim
        .run(async move {
            let task = spawn("loud", async move {
                let _guard = Loud;
                Timer::new(ticks(5)).await;
            });
            Timer::new(ticks(1)).await;
            task.cancel();
        })
        .unwrap_err();
    assert!(
        matches!(&f.kind, FailureKind::TaskPanic { message } if message.contains("cleanup failed")),
        "{f}"
    );
    assert_eq!(f.task.as_deref(), Some("main.loud"));
    assert_eq!(f.location.unwrap().line(), line);
}

#[test]
fn model_fatal_and_finish_stop_the_run() {
    for finish in [false, true] {
        let mut m = FakeModel::new(Precision::NS);
        let go = m.signal("go", 1);
        m.comb(move |s| {
            if s.get(go) == 1 {
                if finish {
                    s.finish();
                } else {
                    s.fatal("assertion failed in top.chk");
                }
            }
        });
        let mut sim = Sim::from_fake(m);
        let go = sim.signal("go").unwrap();
        let f = sim
            .run(async move {
                go.write_u64(1);
                Timer::new(ticks(5)).await;
            })
            .unwrap_err();
        let expect = if finish {
            FailureKind::DesignFinished
        } else {
            FailureKind::ModelFatal {
                message: "assertion failed in top.chk".into(),
            }
        };
        assert_eq!(f.kind, expect);
        assert_clean(&sim);
    }
}

#[test]
fn time_limit_stops_a_run_that_would_go_on_forever() {
    let mut d = clocked();
    d.sim.set_time_limit(SimTime(100));
    let e = d.sim.run(async move { Event::new("never").wait().await });
    let f = e.unwrap_err();
    assert_eq!(
        f.kind,
        FailureKind::TimeLimit {
            limit: SimTime(100),
            blocked: vec![BlockedTask {
                task: "main".into(),
                awaiting: vec!["Event `never`".into()],
            }],
        }
    );
    assert!(
        f.to_string().contains("task `main` awaits Event `never`"),
        "{f}"
    );
    assert!(d.sim.now() <= SimTime(100));
    assert_clean(&d.sim);
}

#[test]
fn foreign_and_stale_handles_panic_naming_the_signal() {
    let other = dut();
    let foreign = other.a;
    let mut d = dut();
    let f = d.sim.run(async move { foreign.read_u64() }).unwrap_err();
    let FailureKind::TaskPanic { message } = &f.kind else {
        panic!("{f}")
    };
    assert!(
        message.contains("signal `a` belongs to simulation"),
        "{message}"
    );

    let outside = panic::catch_unwind(AssertUnwindSafe(|| d.sum.read_u64())).unwrap_err();
    let message = outside.downcast_ref::<String>().unwrap();
    assert!(
        message.contains("signal `sum` used outside a running simulation"),
        "{message}"
    );
}

#[test]
fn values_that_do_not_fit_are_rejected_at_the_call() {
    let mut d = dut();
    let a = d.a;
    let f = d.sim.run(async move { a.write_u64(0x100) }).unwrap_err();
    let FailureKind::TaskPanic { message } = &f.kind else {
        panic!("{f}")
    };
    assert!(
        message.contains("does not fit signal `a` (8 bits)"),
        "{message}"
    );
}

#[test]
fn a_panicking_fake_model_closure_is_a_model_fatal() {
    let mut m = FakeModel::new(Precision::NS);
    let a = m.signal("a", 8);
    m.comb(move |s| assert!(s.get(a) != 3, "a must never be 3"));
    let mut sim = Sim::from_fake(m);
    let a = sim.signal("a").unwrap();
    let f = sim
        .run(async move {
            a.write_u64(3);
            Timer::new(ticks(1)).await;
        })
        .unwrap_err();
    let FailureKind::ModelFatal { message } = &f.kind else {
        panic!("{f}")
    };
    assert!(message.contains("a must never be 3"), "{message}");
    assert!(
        message.contains("failures.rs"),
        "names the panicking line: {message}"
    );
    assert_clean(&sim);
}

#[test]
fn a_run_nested_inside_a_combinator_operand_is_refused_by_name() {
    // A combinator polls its operands while it holds the current kernel; a nested run
    // still fails with the "cannot be nested" message, not a borrow error.
    let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
    let f = sim
        .run(async {
            simjig::first!(
                async {
                    let mut inner = Sim::from_fake(FakeModel::new(Precision::NS));
                    drop(inner.run(async {}));
                },
                Timer::new(ticks(5))
            );
        })
        .unwrap_err();
    assert!(matches!(f.kind, FailureKind::TaskPanic { .. }), "{f}");
    assert!(
        f.message
            .contains("Sim::run cannot be nested inside another Sim::run"),
        "{f}"
    );
}

/// `with_timeout(..).await?` in a body returning `simjig::Result`: the `Elapsed` becomes a
/// `Timeout` failure of the awaiting task, printed in physical units.
#[test]
fn an_elapsed_timeout_converts_into_a_failure_of_the_awaiting_task() {
    let mut d = dut();
    let out = d
        .sim
        .run(async {
            Timer::new(ticks(5)).await;
            simjig::scope("waiter", async {
                let never = Event::new("never");
                simjig::with_timeout(ticks(400_000), never.wait()).await?;
                simjig::Result::Ok(())
            })
            .await
        })
        .unwrap();
    let f: simjig::Failure = out.unwrap_err();
    assert_eq!(
        f.kind,
        FailureKind::Timeout {
            after: ticks(400_000)
        }
    );
    assert_eq!(f.time, SimTime(400_005));
    assert_eq!(f.task.as_deref(), Some("main.waiter"));
    assert_eq!(f.message, "timed out after 400 us");
    assert!(
        f.to_string().contains("simjig Timeout at t=400005 ns"),
        "{f}"
    );
    assert_clean(&d.sim);
}
