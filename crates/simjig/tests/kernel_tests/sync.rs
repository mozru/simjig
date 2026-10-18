//! `Event`, `Lock` and `Queue`: ordering, back-pressure and cancellation safety.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::{Event, Lock, Queue, Timer, first, now, spawn};

use super::{assert_clean, dut, ticks};

type Trace = Rc<RefCell<Vec<String>>>;

#[test]
fn event_wakes_all_waiters_in_wait_order_and_a_set_event_returns_at_once() {
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
            e.wait().await;
            t.borrow_mut().push("already set".into());
        })
        .unwrap();
    assert_eq!(*trace.borrow(), ["w1 at 4", "w2 at 4", "already set"]);
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
