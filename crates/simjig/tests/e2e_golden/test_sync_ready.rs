//! `tests/conformance/tb/test_sync_ready.py`: where a task resumes after awaiting an
//! `Event`, a `Lock` or a `Queue`, ready or not, among the other tasks one edge wakes.

use std::future::Future;

use simjig::{Event, In, Lock, Queue, Task, first, ns, spawn, timer};
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
                                timer(ns(1)).await;
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
            pa.wait(timer(ns(2)), "Timer(2ns)").await;
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

#[simjig::test]
async fn queue_get_ready(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    // cocotb's `Queue()` is unbounded; this one never fills.
    let q: Queue<u32> = Queue::new("q", 8);
    counter_start(&mut p, dut).await; // 10 ns

    // 15 ns: the item is there; a resumes before b.
    q.try_put(5).unwrap();
    let qa = q.clone();
    join_all(vec![
        on_edge(&log, clk, "a", move |pa| async move {
            pa.note("item", &qa.get().await.to_string());
            s(&pa);
        }),
        on_edge(&log, clk, "b", move |pb| async move { s(&pb) }),
    ])
    .await;

    // 25 ns: ga and gc wait; s puts 1, which wakes ga, but g runs first and takes it, so
    // ga waits again behind gc. The items put at 27 ns go to gc, then ga.
    let getter = |name: &'static str| {
        let mut pg = log.probe(name);
        let q = q.clone();
        spawn(name, async move {
            let item = pg.wait(q.get(), "Queue.get()").await;
            pg.note("item", &item.to_string());
            s(&pg);
        })
    };
    let (tga, tgc) = (getter("ga"), getter("gc"));
    let (qs, qg) = (q.clone(), q.clone());
    let ts = vec![
        on_edge(&log, clk, "s", move |ps| async move {
            qs.try_put(1).unwrap();
            s(&ps);
        }),
        on_edge(&log, clk, "g", move |pg| async move {
            pg.note("item", &qg.get().await.to_string());
            s(&pg);
        }),
        on_edge(&log, clk, "t", move |pt| async move { s(&pt) }),
        tga,
        tgc,
    ];
    p.wait(timer(ns(12)), "Timer(12ns)").await; // 27 ns
    q.try_put(2).unwrap();
    q.try_put(3).unwrap();
    join_all(ts).await;
    log.check("queue_get_ready");
}

#[simjig::test]
async fn queue_put_ready(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    let q: Queue<u32> = Queue::new("q", 1);
    counter_start(&mut p, dut).await; // 10 ns

    // 15 ns: there is room; a resumes before b. main empties the queue at 16 ns.
    let qa = q.clone();
    let ts = vec![
        on_edge(&log, clk, "a", move |pa| async move {
            qa.put(1).await;
            pa.note("put", "1");
            s(&pa);
        }),
        on_edge(&log, clk, "b", move |pb| async move { s(&pb) }),
    ];
    p.wait(timer(ns(6)), "Timer(6ns)").await; // 16 ns
    p.note("item", &q.try_get().unwrap().to_string());
    join_all(ts).await;

    // 25 ns: the queue is full; pa and pc wait. g takes the item, which wakes pa, but pb
    // runs first and fills the slot, so pa waits again behind pc. main empties the queue
    // at 27, 29 and 31 ns: 13, then 12 (pc), then 11 (pa).
    q.try_put(10).unwrap();
    let putter = |name: &'static str, item: u32| {
        let mut pp = log.probe(name);
        let q = q.clone();
        spawn(name, async move {
            pp.wait(q.put(item), "Queue.put()").await;
            pp.note("put", &item.to_string());
            s(&pp);
        })
    };
    let (tpa, tpc) = (putter("pa", 11), putter("pc", 12));
    let (qg, qb) = (q.clone(), q.clone());
    let ts = vec![
        on_edge(&log, clk, "g", move |pg| async move {
            pg.note("item", &qg.try_get().unwrap().to_string());
            s(&pg);
        }),
        on_edge(&log, clk, "pb", move |pp| async move {
            qb.put(13).await;
            pp.note("put", "13");
            s(&pp);
        }),
        on_edge(&log, clk, "t", move |pt| async move { s(&pt) }),
        tpa,
        tpc,
    ];
    p.wait(timer(ns(11)), "Timer(11ns)").await; // 27 ns
    for _ in 0..3 {
        p.note("item", &q.try_get().unwrap().to_string());
        p.wait(timer(ns(2)), "Timer(2ns)").await;
    }
    join_all(ts).await;
    log.check("queue_put_ready");
}
