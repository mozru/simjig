//! `Event`, `Lock` and `Queue`: ordering, back-pressure and cancellation safety.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::{Event, Lock, Queue, Timer, first, now, spawn};

use super::{assert_clean, dut, ticks};

type Trace = Rc<RefCell<Vec<String>>>;

#[test]
fn event_wakes_all_waiters_in_wait_order_and_a_set_event_resumes_after_the_ready_tasks() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let e = Event::new("go");
            let mut waiters = Vec::new();
            for name in ["w1", "w2"] {
                let (e, t) = (e.clone(), Rc::clone(&t));
                waiters.push(spawn(name, async move {
                    e.wait().await;
                    t.borrow_mut().push(format!("{name} at {}", now().ticks()));
                }));
            }
            Timer::new(ticks(4)).await;
            e.set();
            e.clear(); // woken waiters still resume
            for w in waiters {
                w.await;
            }
            e.set();
            let (e2, t2) = (e.clone(), Rc::clone(&t));
            let other = spawn("other", async move {
                e2.clear(); // a set event already fired for main
                t2.borrow_mut().push("other".into());
            });
            e.wait().await; // cocotb: one hop, behind `other`
            t.borrow_mut().push("already set".into());
            other.await;
        })
        .unwrap();
    assert_eq!(
        *trace.borrow(),
        ["w1 at 4", "w2 at 4", "other", "already set"]
    );
    assert_clean(&d.sim);
}

#[test]
fn lock_is_granted_in_arrival_order_and_passed_on_by_a_cancelled_waiter() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let lock = Lock::new("bus");
            let guard = lock.lock().await;
            let mut tasks = Vec::new();
            for name in ["a", "b", "c"] {
                let (lock, t) = (lock.clone(), Rc::clone(&t));
                tasks.push(spawn(name, async move {
                    let _g = lock.lock().await;
                    t.borrow_mut().push(format!("{name} at {}", now().ticks()));
                    Timer::new(ticks(2)).await;
                }));
            }
            Timer::new(ticks(1)).await;
            assert!(lock.try_lock().is_none());
            // Release and cancel `a` (the next in line) in the same delta: `a` is granted
            // the lock but never resumes, so it must hand it to `b`.
            drop(guard);
            let a = tasks.remove(0);
            drop(a);
            for task in tasks {
                task.await;
            }
            assert!(!lock.is_locked());
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["b at 1", "c at 3"]);
    assert_clean(&d.sim);
}

#[test]
fn a_free_lock_is_taken_at_once_and_its_task_resumes_after_the_ready_tasks() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let lock = Lock::new("bus");
            let (l2, t2) = (lock.clone(), Rc::clone(&t));
            let other = spawn("other", async move {
                let locked = l2.is_locked();
                t2.borrow_mut().push(format!("other sees locked={locked}"));
            });
            let guard = lock.lock().await; // cocotb: taken now, resumed one hop later
            t.borrow_mut().push("main has it".into());
            other.await;
            drop(guard);
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["other sees locked=true", "main has it"]);
    assert_clean(&d.sim);
}

#[test]
fn queue_applies_back_pressure_and_keeps_fifo_order() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let q = Queue::new("fifo", 2);
            let (qp, tp) = (q.clone(), Rc::clone(&t));
            let producer = spawn("producer", async move {
                for i in 0..5 {
                    qp.put(i).await;
                    tp.borrow_mut()
                        .push(format!("put {i} at {}", now().ticks()));
                }
            });
            for _ in 0..5 {
                Timer::new(ticks(10)).await;
                let v = q.get().await;
                t.borrow_mut().push(format!("got {v} at {}", now().ticks()));
            }
            producer.await;
            assert!(q.try_get().is_none());
            assert_eq!(q.try_put(9), Ok(()));
            assert_eq!(q.try_put(10), Ok(()));
            assert_eq!(q.try_put(11), Err(11));
        })
        .unwrap();
    let trace = trace.borrow();
    assert_eq!(
        trace[..4],
        ["put 0 at 0", "put 1 at 0", "got 0 at 10", "put 2 at 10"]
    );
    let got: Vec<_> = trace.iter().filter(|s| s.starts_with("got")).collect();
    assert_eq!(got.len(), 5);
    assert!(
        got.iter()
            .enumerate()
            .all(|(i, s)| s.starts_with(&format!("got {i} ")))
    );
    assert_clean(&d.sim);
}

#[test]
fn a_get_that_loses_a_race_loses_no_item() {
    let mut d = dut();
    let out = d.sim.run(async {
        let q = Queue::new("q", 1);
        let qp = q.clone();
        let _producer = spawn("producer", async move {
            Timer::new(ticks(5)).await;
            qp.put(7).await;
        });
        // The timeout wins at 5 (timers run first), dropping the pending get.
        let first_try = first!(
            async {
                let v = q.get().await;
                Some(v)
            },
            async {
                Timer::new(ticks(5)).await;
                None
            },
        );
        Timer::new(ticks(1)).await;
        (first_try, q.try_get())
    });
    assert_eq!(out.unwrap(), (None, Some(7)));
    assert_clean(&d.sim);
}

/// cocotb's `Queue.get()` loops on `while self.empty()`: a get that finds an item takes it
/// at once, even when a woken getter has not run yet; that one waits again, last in line.
#[test]
fn a_get_that_finds_an_item_takes_it_ahead_of_a_woken_getter() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let q = Queue::new("q", 4);
            let mut getters = Vec::new();
            for name in ["ga", "gc"] {
                let (q, t) = (q.clone(), Rc::clone(&t));
                getters.push(spawn(name, async move {
                    let v = q.get().await;
                    t.borrow_mut().push(format!("{name} got {v}"));
                }));
            }
            Timer::new(ticks(1)).await;
            q.try_put(1).unwrap(); // wakes ga
            let v = q.get().await; // runs before ga
            t.borrow_mut().push(format!("main got {v}"));
            Timer::new(ticks(1)).await; // ga found the queue empty and waits behind gc
            q.try_put(2).unwrap();
            q.try_put(3).unwrap();
            for g in getters {
                g.await;
            }
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["main got 1", "gc got 2", "ga got 3"]);
    assert_clean(&d.sim);
}

/// The same for `put`: a put that finds room fills it ahead of a woken putter.
#[test]
fn a_put_that_finds_room_fills_it_ahead_of_a_woken_putter() {
    let mut d = dut();
    let trace: Trace = Rc::default();
    let t = Rc::clone(&trace);
    d.sim
        .run(async move {
            let q = Queue::new("q", 1);
            q.try_put(10).unwrap();
            let mut putters = Vec::new();
            for (name, v) in [("pa", 11), ("pc", 12)] {
                let q = q.clone();
                putters.push(spawn(name, async move { q.put(v).await }));
            }
            Timer::new(ticks(1)).await;
            let mut got = vec![q.try_get().unwrap()]; // wakes pa
            q.put(13).await; // runs before pa
            for _ in 0..3 {
                Timer::new(ticks(1)).await;
                got.push(q.try_get().unwrap());
            }
            t.borrow_mut().extend(got.iter().map(ToString::to_string));
            for p in putters {
                p.await;
            }
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["10", "13", "12", "11"]);
    assert_clean(&d.sim);
}

/// Conformance README, "Known differences": a lock taken by a `first!` loser that is
/// cancelled before it resumes is passed on. Here the loser takes the free lock and is
/// cancelled during its hop; cocotb 2.1 leaves the lock held by the cancelled helper for
/// good (seen in an experiment: `locked()` stays true and a later acquire times out).
#[test]
fn a_lock_taken_by_a_cancelled_first_loser_is_released() {
    let mut d = dut();
    let out = d.sim.run(async {
        let (ev, lock) = (Event::new("ev"), Lock::new("bus"));
        ev.set();
        let won = first!(
            async {
                ev.wait().await;
                "ev"
            },
            async {
                drop(lock.lock().await);
                "lock"
            },
        );
        (won, lock.is_locked(), lock.try_lock().is_some())
    });
    assert_eq!(out.unwrap(), ("ev", false, true));
    assert_clean(&d.sim);
}

/// Conformance README, "Known differences": a `first!` loser woken by a `put`, then
/// cancelled before it runs, passes the wake to the next getter. cocotb 2.1 loses it (seen
/// in an experiment: the next getter never got the item, which stayed in the queue).
#[test]
fn a_queue_wake_of_a_cancelled_first_loser_goes_to_the_next_getter() {
    let mut d = dut();
    let out = d.sim.run(async {
        let (ev, q) = (Event::new("ev"), Queue::new("q", 1));
        let (qn, qp, set) = (q.clone(), q.clone(), ev.clone());
        let next = spawn("next", async move {
            Timer::new(ticks(1)).await; // waits behind the loser
            qn.get().await
        });
        let _x = spawn("x", async move {
            Timer::new(ticks(2)).await;
            set.set(); // wakes the deciding operand first
            qp.try_put(7).unwrap(); // then the getter that will lose
        });
        let won = first!(
            async {
                ev.wait().await;
                "ev"
            },
            async {
                q.get().await;
                "get"
            },
        );
        (won, next.await, q.len())
    });
    assert_eq!(out.unwrap(), ("ev", 7, 0));
    assert_clean(&d.sim);
}

/// A capacity of 0 is unbounded, as cocotb's `Queue(maxsize=0)`: `put` never waits.
#[test]
fn a_queue_of_capacity_zero_is_unbounded() {
    let mut d = dut();
    d.sim
        .run(async move {
            let q = Queue::new("unbounded", 0);
            for i in 0..1000 {
                q.put(i).await;
            }
            assert_eq!(now().ticks(), 0, "no put waited");
            assert_eq!(q.try_put(1000), Ok(()));
            assert_eq!((q.len(), q.capacity()), (1001, 0));
            assert_eq!(q.get().await, 0);
        })
        .unwrap();
    assert_clean(&d.sim);
}
