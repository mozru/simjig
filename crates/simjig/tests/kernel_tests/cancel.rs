//! Cancellation is dropping: cleanup runs in `Drop`, triggers deregister, scopes cancel
//! their subtree.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::{Timer, TraceKind, rising, scope, spawn, with_timeout};

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
        Timer::new(ticks(12)).await;
        let during = a.read_u64();
        drop(driver);
        Timer::new(ticks(1)).await;
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
                Timer::new(ticks(25)).await; // edges at 0, 10, 20
            });
            parent.await;
            Timer::new(ticks(100)).await; // the ticker must be gone by now
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
                Timer::new(ticks(1000)).await;
            }),
        )
        .await;
        Timer::new(ticks(1)).await;
        (result.is_err(), a.read_u64(), simjig::now().ticks())
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
                Timer::new(ticks(1000)).await;
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
            Timer::new(ticks(3)).await;
            2
        });
        Timer::new(ticks(1)).await;
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
            Timer::new(ticks(30)).await;
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
                Timer::new(ticks(100)).await;
            });
            Timer::new(ticks(1)).await;
            drop(t);
            Timer::new(ticks(1)).await;
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
                Timer::new(ticks(100)).await;
            });
            Timer::new(ticks(1)).await;
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
fn a_handle_dropped_before_its_task_ever_ran_is_recorded() {
    let mut d = dut();
    let ran = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&ran);
    d.sim
        .run(async move {
            // The classic mistake: `let _ = spawn(..)` cancels the task at once.
            drop(spawn("lost", async move { *flag.borrow_mut() = true }));
            // An explicit cancel is deliberate and is not recorded.
            spawn("cancelled", async {}).cancel();
            Timer::new(ticks(1)).await;
        })
        .unwrap();
    assert!(!*ran.borrow(), "the dropped task never ran");
    let dropped: Vec<_> = d
        .sim
        .recent_trace()
        .into_iter()
        .filter(|r| r.kind == TraceKind::DroppedUnpolled)
        .collect();
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert_clean(&d.sim);
}
