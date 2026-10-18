//! `tests/conformance/tb/test_sync_ready.py`: where a task resumes after awaiting an
//! `Event`, a `Lock` or a `Queue`, ready or not, among the other tasks one edge wakes.

use std::future::Future;

use simjig::{Event, In, Lock, Task, Timer, first, ns, spawn};
use simjig_bits::Bit;

use crate::conflog::{Log, Probe};
use crate::test_callback_order::{counter_start, seen};

type Dut = crate::counter::Dut;

/// A task that waits on `RisingEdge(clk)`, then runs `body` (the module's `edge_tasks`).
fn on_edge<F: Future<Output = ()> + 'static>(
    log: &Log,
    clk: In<Bit>,
    name: &'static str,
    body: impl FnOnce(Probe) -> F + 'static,
) -> Task<()> {
    let mut p = log.probe(name);
    spawn(name, async move {
        p.wait(clk.rising(), "RisingEdge(clk)").await;
        body(p).await;
    })
}

async fn join_all(tasks: Vec<Task<()>>) {
    for t in tasks {
        t.await;
    }
}

fn locked(lock: &Lock) -> &'static str {
    if lock.is_locked() { "True" } else { "False" }
}

#[simjig::test]
async fn event_ready(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    let ev = Event::new("ev");
    counter_start(&mut p, dut).await; // 10 ns

    // 15 ns: a awaits the set Event and resumes after b and c; b clears it in between.
    ev.set();
    let (wait, clear) = (ev.clone(), ev.clone());
    join_all(vec![
        on_edge(&log, clk, "a", move |mut pa| async move {
            pa.wait(wait.wait(), "Event.wait()").await;
            s(&pa);
        }),
        on_edge(&log, clk, "b", move |pb| async move {
            s(&pb);
            clear.clear();
        }),
        on_edge(&log, clk, "c", move |pc| async move { s(&pc) }),
    ])
    .await;

    // 25 ns: w waits on the cleared Event; s sets it, and w resumes after t.
    let mut pw = log.probe("w");
    let (wait, set) = (ev.clone(), ev.clone());
    let tw = spawn("w", async move {
        pw.wait(wait.wait(), "Event.wait()").await;
        s(&pw);
    });
    join_all(vec![
        on_edge(&log, clk, "s", move |ps| async move {
            s(&ps);
            set.set();
        }),
        on_edge(&log, clk, "t", move |pt| async move { s(&pt) }),
        tw,
    ])
    .await;

    // 35 ns: the set Event inside a First, in a helper task that also takes a hop.
    let wait = ev.clone();
    join_all(vec![
        on_edge(&log, clk, "d", move |mut pd| async move {
            let won = pd
                .wait(
                    async {
                        first!(
                            async {
                                wait.wait().await;
                                "ev"
                            },
                            async {
                                Timer::new(ns(1)).await;
                                "timer"
                            }
                        )
                    },
                    "First",
                )
                .await;
            pd.note("first", won);
            s(&pd);
        }),
        on_edge(&log, clk, "e", move |pe| async move { s(&pe) }),
    ])
    .await;
    log.check("event_ready");
}

#[simjig::test]
async fn lock_ready(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    let lock = Lock::new("lock");
    counter_start(&mut p, dut).await; // 10 ns

    // 15 ns: a takes the free lock and resumes after b and c; b sees it locked, c waits.
    // a releases at 17 ns; c resumes after the rest of a.
    let (la, lb, lc) = (lock.clone(), lock.clone(), lock.clone());
    join_all(vec![
        on_edge(&log, clk, "a", move |mut pa| async move {
            let guard = pa.wait(la.lock(), "Lock.acquire()").await;
            s(&pa);
            pa.wait(Timer::new(ns(2)), "Timer(2ns)").await;
            drop(guard);
            pa.note("locked", locked(&la));
        }),
        on_edge(&log, clk, "b", move |pb| async move {
            pb.note("locked", locked(&lb));
            s(&pb);
        }),
        on_edge(&log, clk, "c", move |mut pc| async move {
            let guard = pc.wait(lc.lock(), "Lock.acquire()").await;
            s(&pc);
            drop(guard);
        }),
    ])
    .await;

    // 25 ns: main holds the lock and releases it on the edge; w, waiting, resumes after
    // the other tasks the edge woke.
    let guard = lock.lock().await;
    let mut pw = log.probe("w");
    let lw = lock.clone();
    let tw = spawn("w", async move {
        let guard = pw.wait(lw.lock(), "Lock.acquire()").await;
        s(&pw);
        drop(guard);
    });
    let tr = on_edge(&log, clk, "r", move |pr| async move { s(&pr) });
    p.wait(clk.rising(), "RisingEdge(clk)").await;
    drop(guard);
    s(&p);
    join_all(vec![tr, tw]).await;
    log.check("lock_ready");
}
