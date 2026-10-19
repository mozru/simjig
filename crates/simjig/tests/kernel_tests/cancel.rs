//! Cancellation is dropping: cleanup runs in `Drop`, triggers deregister, scopes cancel
//! their subtree.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::{
    FailureKind, TraceKind, first, next_time_step, now, rising, scope, spawn, timer, with_timeout,
};

use super::{assert_clean, clocked, dut, ticks};

/// Drives `valid` high while alive and low again when dropped, like a bus driver guard.
struct DriveWhileAlive(simjig::RawSig);

impl DriveWhileAlive {
    fn new(sig: simjig::RawSig) -> Self {
        sig.write_u64(1);
        Self(sig)
    }
}

impl Drop for DriveWhileAlive {
    fn drop(&mut self) {
        self.0.write_u64(0);
    }
}

/// Sets its flag when dropped, to observe that a task's future was dropped.
struct SetOnDrop(Rc<RefCell<bool>>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        *self.0.borrow_mut() = true;
    }
}

#[test]
fn dropping_a_task_handle_cancels_it_and_runs_drop_cleanup() {
    let mut d = clocked();
    let (clk, a) = (d.clk, d.a);
    let seen = d.sim.run(async move {
        let driver = spawn("driver", async move {
            let _guard = DriveWhileAlive::new(a);
            loop {
                rising(clk).await;
            }
        });
        timer(ticks(12)).await;
        let during = a.read_u64();
        drop(driver);
        timer(ticks(1)).await;
        (during, a.read_u64())
    });
    assert_eq!(
        seen.unwrap(),
        (1, 0),
        "the guard's write went through the write log"
    );
    assert_clean(&d.sim);
}

#[test]
fn detached_children_die_with_their_parent() {
    let mut d = clocked();
    let clk = d.clk;
    let ticks_seen = Rc::new(RefCell::new(0));
    let seen = Rc::clone(&ticks_seen);
    d.sim
        .run(async move {
            let parent = spawn("parent", async move {
                spawn("ticker", async move {
                    loop {
                        rising(clk).await;
                        *seen.borrow_mut() += 1;
                    }
                })
                .detach();
                timer(ticks(25)).await; // edges at 0, 10, 20
            });
            parent.await;
            timer(ticks(100)).await; // the ticker must be gone by now
        })
        .unwrap();
    assert_eq!(*ticks_seen.borrow(), 3);
    assert_clean(&d.sim);
}

#[test]
fn a_timed_out_scope_cancels_its_whole_subtree() {
    let mut d = clocked();
    let (clk, a) = (d.clk, d.a);
    let out = d.sim.run(async move {
        let result = with_timeout(
            ticks(15),
            scope("work", async move {
                spawn("deep", async move {
                    let _guard = DriveWhileAlive::new(a);
                    loop {
                        rising(clk).await;
                    }
                })
                .detach();
                timer(ticks(1000)).await;
            }),
        )
        .await;
        timer(ticks(1)).await;
        (result.is_err(), a.read_u64(), now().ticks())
    });
    assert_eq!(out.unwrap(), (true, 0, 16));
    assert_clean(&d.sim);
}

/// A detached task whose spawner is already gone still belongs to the nearest live
/// ancestor (here the scope), so the scope's timeout cancels it.
#[test]
fn a_task_detached_after_its_spawner_finished_dies_with_the_scope() {
    let mut d = clocked();
    let clk = d.clk;
    let cleaned = Rc::new(RefCell::new(false));
    let flag = SetOnDrop(Rc::clone(&cleaned));
    let seen = Rc::clone(&cleaned);
    d.sim.set_time_limit(simjig::SimTime(1000));
    let out = d.sim.run(async move {
        let result = with_timeout(
            ticks(15),
            scope("s", async move {
                // `p` returns `c`'s handle, so `p` is freed before `c` is detached.
                #[expect(clippy::async_yields_async, reason = "the handle is the result")]
                let c = spawn("p", async move {
                    spawn("c", async move {
                        let _flag = flag;
                        loop {
                            rising(clk).await;
                        }
                    })
                })
                .await;
                c.detach();
                timer(ticks(1000)).await;
            }),
        )
        .await;
        (result.is_err(), *seen.borrow())
    });
    assert_eq!(out.unwrap(), (true, true), "`c` was dropped with the scope");
    assert_clean(&d.sim);
}

#[test]
fn awaiting_a_task_returns_its_result_even_if_already_done() {
    let mut d = dut();
    let out = d.sim.run(async {
        let quick = spawn("quick", async { 40 });
        let slow = spawn("slow", async {
            timer(ticks(3)).await;
            2
        });
        timer(ticks(1)).await;
        assert!(quick.is_finished());
        assert_eq!(quick.name(), "main.quick");
        quick.await + slow.await
    });
    assert_eq!(out.unwrap(), 42);
    assert_clean(&d.sim);
}

#[test]
fn tasks_left_running_are_cancelled_when_main_returns() {
    let mut d = clocked();
    let (clk, a) = (d.clk, d.a);
    let cleaned = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&cleaned);
    d.sim
        .run(async move {
            spawn("forever", async move {
                let _flag = SetOnDrop(flag);
                let _guard = DriveWhileAlive::new(a);
                loop {
                    rising(clk).await;
                }
            })
            .detach();
            timer(ticks(30)).await;
        })
        .unwrap();
    assert!(*cleaned.borrow());
    assert_clean(&d.sim);
}

#[test]
fn a_panic_in_drop_cleanup_is_a_task_panic() {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("cleanup failed");
        }
    }
    let mut d = dut();
    let f = d
        .sim
        .run(async {
            let t = spawn("fragile", async {
                let _p = PanicOnDrop;
                timer(ticks(100)).await;
            });
            timer(ticks(1)).await;
            drop(t);
            timer(ticks(1)).await;
        })
        .unwrap_err();
    assert_eq!(f.task.as_deref(), Some("main.fragile"));
    assert!(f.message.contains("cleanup failed"), "{f}");
    assert_clean(&d.sim);
}

#[test]
fn a_cleanup_panic_while_unwinding_does_not_mask_the_first_panic() {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("cleanup failed");
        }
    }
    let mut d = dut();
    let f = d
        .sim
        .run(async {
            let _b = spawn("b", async {
                let _p = PanicOnDrop;
                timer(ticks(100)).await;
            });
            timer(ticks(1)).await;
            // Unwinding out of `main` drops `_b`, whose cleanup panics in turn.
            panic!("main failed");
        })
        .unwrap_err();
    assert_eq!(f.task.as_deref(), Some("main"), "{f}");
    assert!(f.message.contains("main failed"), "{f}");
    let location = f.location.unwrap();
    assert!(location.file().ends_with("cancel.rs"), "{f}");
    assert_eq!(location.line(), line!() - 7, "the `panic!` line: {f}");
    assert_clean(&d.sim);
}

#[test]
fn a_handle_dropped_before_its_task_ever_ran_fails_the_run_at_the_spawn() {
    let mut d = dut();
    let ran = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&ran);
    let f = d
        .sim
        .run(async move {
            // The classic mistake: `let _ = spawn(..)` cancels the task at once, so the
            // driver never runs and the test would pass without it.
            #[expect(clippy::let_underscore_future, reason = "the mistake under test")]
            let _ = spawn("lost", async move { *flag.borrow_mut() = true });
            timer(ticks(1)).await;
        })
        .unwrap_err();
    let spawn_line = line!() - 4;
    assert!(!*ran.borrow(), "the dropped task never ran");
    let FailureKind::Setup { message, help } = &f.kind else {
        panic!("{f}")
    };
    assert!(message.contains("`main.lost`"), "{f}");
    assert!(message.contains("let _ = spawn(..)"), "{f}");
    for fix in [".detach()", ".await", ".cancel()"] {
        assert!(help.contains(fix), "{fix}: {f}");
    }
    assert_eq!(f.task.as_deref(), Some("main"), "{f}");
    let location = f.location.unwrap();
    assert!(location.file().ends_with("cancel.rs"), "{f}");
    assert_eq!(location.line(), spawn_line, "the `spawn` line: {f}");
    let dropped = d
        .sim
        .recent_trace()
        .into_iter()
        .filter(|r| r.kind == TraceKind::DroppedUnpolled)
        .count();
    assert_eq!(dropped, 1);
    assert_clean(&d.sim);
}

/// `let a = spawn(..); let b = spawn(..); check?;` inside a helper whose caller handles the
/// `Err` and goes on: both handles are dropped together, and the one failure names both,
/// says how a handle gets dropped early, and names the fixes.
#[test]
fn handles_dropped_unrun_together_are_all_named_in_one_failure() {
    async fn setup() -> Result<(), &'static str> {
        let a = spawn("a", async {});
        let b = spawn("b", async {});
        Err("not needed")?;
        a.await;
        b.await;
        Ok(())
    }
    let mut d = dut();
    let f = d
        .sim
        .run(async move {
            assert!(setup().await.is_err());
            timer(ticks(1)).await;
        })
        .unwrap_err();
    let FailureKind::Setup { message, help } = &f.kind else {
        panic!("{f}")
    };
    for part in [
        "`main.a`",
        "`main.b`",
        "were dropped before the tasks ran, so they were cancelled",
        "let _ = spawn(..)",
        "leaving the handle's scope early",
    ] {
        assert!(message.contains(part), "{part}: {f}");
    }
    for fix in [".detach()", ".await", ".cancel()"] {
        assert!(help.contains(fix), "{fix}: {f}");
    }
    assert_clean(&d.sim);
}

/// An early `return` from `main` after deciding a monitor is not needed ends the run: the
/// monitor would not have run even if detached, so the run passes.
#[test]
fn an_early_return_from_main_drops_an_unrun_handle_without_failing() {
    let mut d = dut();
    let needed = false;
    let out = d.sim.run(async move {
        timer(ticks(1)).await;
        let monitor = spawn("monitor", timer(ticks(10)));
        if !needed {
            return 7;
        }
        monitor.await;
        0
    });
    assert_eq!(out.unwrap(), 7);
    assert_clean(&d.sim);
}

/// The exemption is `main`'s alone: a child that drops an unrun handle and returns in the
/// same poll still fails, since a detached grandchild would have run under `main`.
#[test]
fn a_child_that_returns_after_dropping_an_unrun_handle_fails() {
    let mut d = dut();
    let ran = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&ran);
    let f = d
        .sim
        .run(async move {
            spawn("child", async move {
                #[expect(clippy::let_underscore_future, reason = "the mistake under test")]
                let _ = spawn("lost", async move { *flag.borrow_mut() = true });
            })
            .await;
            timer(ticks(1)).await;
        })
        .unwrap_err();
    assert!(!*ran.borrow());
    assert!(matches!(f.kind, FailureKind::Setup { .. }), "{f}");
    assert!(f.message.contains("`main.child.lost`"), "{f}");
    assert_eq!(f.task.as_deref(), Some("main.child"), "{f}");
    assert_clean(&d.sim);
}

#[test]
fn deliberate_and_cascading_drops_of_a_task_that_never_ran_pass() {
    let mut d = dut();
    d.sim
        .run(async move {
            // An explicit cancel is deliberate.
            spawn("cancelled", async {}).cancel();
            // A handle dropped after its task ran.
            let ran = spawn("ran", timer(ticks(5)));
            timer(ticks(1)).await;
            drop(ran);
            // A Task passed by value to `first!` loses before it ever runs: the combinator
            // cancels it.
            first!(async {}, async {
                spawn("loser", async {}).await;
            });
            first!(async {}, spawn("loser_by_value", async {}));
            // A task cancelled while it holds the handle of a child that never ran.
            let parent = spawn("parent", async {
                let _child = spawn("child", async {});
                timer(ticks(10)).await;
            });
            timer(ticks(1)).await;
            parent.cancel();
        })
        .unwrap();
    assert_clean(&d.sim);
}

#[test]
fn cancelling_most_waiters_of_one_edge_or_deadline_leaves_the_rest_in_order() {
    // The wait table drops cancelled registrations of an edge group or a timer deadline
    // lazily; the survivors still wake in registration order, and a deadline whose timers
    // were all cancelled schedules no timestep. 200 waiters, 140 cancelled: enough for the
    // stale keys to be compacted away.
    const WAITERS: u64 = 200;
    let kept = |i: &u64| i.is_multiple_of(7) || i % 7 == 1;
    let mut d = clocked();
    let clk = d.clk;
    let order: Rc<RefCell<Vec<String>>> = Rc::default();
    let shared = Rc::clone(&order);
    // Everything is done at t = 10: a survivor that is never woken fails the run at this
    // limit instead of running the free clock until nextest's timeout.
    d.sim.set_time_limit(simjig::SimTime(100));
    d.sim
        .run(async move {
            let mut survivors = Vec::new();
            let mut cancelled = Vec::new();
            for i in 0..WAITERS {
                let log = Rc::clone(&shared);
                let task = spawn("w", async move {
                    if i % 2 == 0 {
                        timer(ticks(5)).await;
                    } else {
                        timer(ticks(2)).await; // past the edge at 0
                        rising(clk).await;
                    }
                    log.borrow_mut().push(format!("{i}@{}", now().ticks()));
                });
                if kept(&i) {
                    survivors.push(task);
                } else {
                    cancelled.push(task);
                }
            }
            let doomed: Vec<_> = (0..20)
                .map(|_| spawn("x", async { timer(ticks(7)).await }))
                .collect();
            timer(ticks(3)).await;
            drop(cancelled);
            drop(doomed);
            timer(ticks(3)).await; // t = 6
            next_time_step().await;
            shared.borrow_mut().push(format!("next@{}", now().ticks()));
            for task in survivors {
                task.await;
            }
        })
        .unwrap();
    let at = |parity: u64, t: u64| {
        (0..WAITERS)
            .filter(move |i| i % 2 == parity && kept(i))
            .map(move |i| format!("{i}@{t}"))
    };
    let want: Vec<String> = at(0, 5)
        .chain(["next@10".to_owned()])
        .chain(at(1, 10))
        .collect();
    assert_eq!(*order.borrow(), want);
    assert_clean(&d.sim);
}

/// Conformance README, "Known differences": a `first!` loser's `Drop` cleanup runs when
/// the decider cancels it, before the tasks the decider's trigger woke after it (`z`).
/// cocotb 2.1 throws `CancelledError` into the loser when it next runs, so its `finally`
/// runs after `z` (seen in an experiment). The caller resumes last in both.
#[test]
fn a_cancelled_losers_drop_cleanup_runs_when_it_is_cancelled() {
    let mut d = clocked();
    let clk = d.clk;
    let trace: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let ev = simjig::Event::new("ev");
            let log = |name| {
                let trace = Rc::clone(&t);
                move || trace.borrow_mut().push(name)
            };
            let (set, wait) = (ev.clone(), ev.clone());
            let (log_x, log_y, log_z) = (log("x"), log("y"), log("z"));
            let tasks = [
                spawn("x", async move {
                    timer(ticks(5)).await;
                    rising(clk).await; // 10
                    set.set();
                    log_x();
                }),
                spawn("y", async move {
                    timer(ticks(5)).await;
                    rising(clk).await;
                    log_y();
                }),
                spawn("z", async move {
                    timer(ticks(1)).await; // waits on `ev` after the operand
                    wait.wait().await;
                    log_z();
                }),
            ];
            let cleanup = log("cleanup");
            simjig::first!(ev.wait(), async move {
                let _guard = OnDrop(Some(cleanup));
                timer(ticks(100)).await;
            });
            log("main")();
            for task in tasks {
                task.await;
            }
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["x", "y", "cleanup", "z", "main"]);
    assert_clean(&d.sim);
}

/// Runs its closure when dropped.
struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// Conformance README, "Known differences" (scenario `r5_task_operand`):
/// a `Task` passed **by value** to `with_timeout` or `first!` is cancelled when the
/// combinator drops it; passed as `&mut task` it keeps running, as a cocotb Task operand
/// does ("the callee continues to run").
#[test]
fn a_task_operand_by_value_is_cancelled_and_by_reference_keeps_running() {
    let mut d = clocked();
    let clk = d.clk;
    let out = d.sim.run(async move {
        // Counts rising edges until its 30th, then returns the count.
        let counter = |name| {
            let edges = Rc::new(RefCell::new(0_u32));
            let seen = Rc::clone(&edges);
            let task = spawn(name, async move {
                for _ in 0..30 {
                    rising(clk).await;
                    *edges.borrow_mut() += 1;
                }
                *edges.borrow()
            });
            (task, seen)
        };

        let (by_value, seen_value) = counter("timeout_by_value");
        let timed_out = with_timeout(ticks(25), by_value).await.is_err();
        let at_timeout = *seen_value.borrow();

        let (mut by_ref, seen_ref) = counter("timeout_by_ref");
        let ref_timed_out = with_timeout(ticks(25), &mut by_ref).await.is_err();

        let (first_value, seen_first_value) = counter("first_by_value");
        simjig::first!(
            async {
                timer(ticks(25)).await;
                0
            },
            first_value
        );
        let at_first = *seen_first_value.borrow();

        let (mut first_ref, seen_first_ref) = counter("first_by_ref");
        simjig::first!(
            async {
                timer(ticks(25)).await;
                0
            },
            &mut first_ref
        );

        timer(ticks(400)).await;
        let after = (
            *seen_value.borrow() - at_timeout,
            *seen_first_value.borrow() - at_first,
        );
        (
            timed_out && ref_timed_out,
            after,
            by_ref.await,
            first_ref.await,
            *seen_ref.borrow() + *seen_first_ref.borrow(),
        )
    });
    assert_eq!(
        out.unwrap(),
        (true, (0, 0), 30, 30, 60),
        "by value: no edge counted after the combinator dropped the task; \
         by reference: the task ran to its 30th edge"
    );
    assert_clean(&d.sim);
}

/// Polls `inner` once with a waker that does nothing, then with the task's: a trigger
/// re-polled with another waker must wake the newest one (`Future::poll`'s contract), as a
/// combinator with wakers of its own, or a future handed to another task, relies on.
struct RepolledWithAnotherWaker<F>(std::pin::Pin<Box<F>>, bool);

impl<F: Future<Output = ()>> Future for RepolledWithAnotherWaker<F> {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if !self.1 {
            self.1 = true;
            let mut noop = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(self.0.as_mut().poll(&mut noop).is_pending());
        }
        self.0.as_mut().poll(cx)
    }
}

#[test]
fn an_edge_repolled_with_another_waker_wakes_the_newest() {
    let mut d = clocked();
    let clk = d.clk;
    let at = d.sim.run(async move {
        timer(ticks(1)).await;
        let woken = RepolledWithAnotherWaker(Box::pin(rising(clk)), false);
        (with_timeout(ticks(100), woken).await.is_ok(), now())
    });
    let (woken, at) = at.unwrap();
    assert!(woken, "the edge woke the first waker, not the newest");
    assert_eq!(at.ticks(), 10);
    assert_clean(&d.sim);
}
