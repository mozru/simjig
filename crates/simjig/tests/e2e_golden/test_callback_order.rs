//! `tests/conformance/tb/test_callback_order.py`: where combinator helpers and their caller
//! run relative to the other tasks one callback wakes, and edge triggers on one signal.

use std::future::Future;

use simjig::{Event, Lock, Queue, Task, Timer, cycles, first, join, ns, spawn, with_timeout};

use crate::conflog::{Log, Probe};
use crate::start_clock;

type Dut = crate::counter::Dut;

pub(crate) async fn counter_start(p: &mut Probe, dut: Dut) {
    dut.clk.set(0);
    start_clock(dut.clk, 10);
    p.write(&dut.rst, 1);
    p.write(&dut.en, 1);
    p.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    p.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    p.write(&dut.rst, 0);
}

pub(crate) fn seen(dut: Dut) -> impl Fn(&Probe) + Copy {
    move |probe: &Probe| probe.read(&[&dut.clk, &dut.count])
}

fn observers(dut: Dut, log: &Log) -> (Task<()>, Task<()>) {
    let ev2 = Event::new("ev2");
    let (mut pf, mut pz) = (log.probe("f_task"), log.probe("z_task"));
    let s = seen(dut);
    let set = ev2.clone();
    let tz = spawn("z_task", async move {
        pz.wait(ev2.wait(), "Event.wait()").await;
        s(&pz);
    });
    let tf = spawn("f_task", async move {
        pf.wait(dut.clk.rising(), "RisingEdge(clk)").await;
        s(&pf);
        set.set();
    });
    (tz, tf)
}

#[simjig::test]
async fn loser_scheduled(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    counter_start(&mut p, dut).await;
    for rnd in 0..2 {
        let ev = Event::new("ev");
        let mut pt = log.probe("t_task");
        let set = ev.clone();
        let tt = spawn("t_task", async move {
            pt.wait(clk.rising(), "RisingEdge(clk)").await;
            s(&pt);
            set.set();
        });
        p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
        let (tz, tf) = observers(dut, &log);
        let res = if rnd == 0 {
            p.wait(
                async {
                    first!(
                        async {
                            ev.wait().await;
                            "ev"
                        },
                        async {
                            clk.rising().await;
                            "rise"
                        }
                    )
                },
                "First",
            )
            .await
        } else {
            p.wait(
                async {
                    first!(
                        async {
                            clk.rising().await;
                            "rise"
                        },
                        async {
                            ev.wait().await;
                            "ev"
                        }
                    )
                },
                "First",
            )
            .await
        };
        p.note("first", res);
        s(&p);
        tt.await;
        tf.await;
        tz.await;
    }
    log.check("loser_scheduled");
}

#[simjig::test]
async fn nested_combinators(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    counter_start(&mut p, dut).await;
    for rnd in 0..3 {
        p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
        let (tz, tf) = observers(dut, &log);
        let label = format!("nested{rnd}");
        match rnd {
            0 => {
                p.wait(
                    async {
                        first!(
                            async {
                                join!(clk.rising(), clk.changed());
                            },
                            Timer::new(ns(100))
                        );
                    },
                    &label,
                )
                .await;
            }
            1 => {
                p.wait(
                    async {
                        join!(
                            async { first!(clk.rising(), Timer::new(ns(100))) },
                            Timer::new(ns(1))
                        );
                    },
                    &label,
                )
                .await;
            }
            _ => {
                p.wait(
                    async {
                        first!(
                            async { first!(clk.rising(), Timer::new(ns(100))) },
                            Timer::new(ns(200))
                        );
                    },
                    &label,
                )
                .await;
            }
        }
        s(&p);
        tf.await;
        tz.await;
    }
    log.check("nested_combinators");
}

#[simjig::test]
async fn timeout_combinators(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    counter_start(&mut p, dut).await;
    for rnd in 0..2 {
        p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
        let (tz, tf) = observers(dut, &log);
        let label = format!("with_timeout{rnd}");
        if rnd == 0 {
            p.wait(
                with_timeout(ns(200), async { first!(clk.rising(), Timer::new(ns(100))) }),
                &label,
            )
            .await
            .unwrap();
        } else {
            p.wait(
                with_timeout(ns(200), async {
                    join!(clk.rising(), clk.changed());
                }),
                &label,
            )
            .await
            .unwrap();
        }
        s(&p);
        tf.await;
        tz.await;
    }
    log.check("timeout_combinators");
}

#[simjig::test]
async fn cycles_join(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let (mut pv, mut pe, mut pc, mut pz) = (
        log.probe("vc_task"),
        log.probe("edge_task"),
        log.probe("cnt_task"),
        log.probe("z_task"),
    );
    let s = seen(dut);
    let (clk, count) = (dut.clk, dut.count);
    let ev = Event::new("ev");
    counter_start(&mut p, dut).await;
    let set = ev.clone();
    let ts = [
        spawn("vc_task", async move {
            pv.wait(clk.changed(), "ValueChange(clk)").await;
            s(&pv);
            set.set();
            pv.wait(cycles(clk, 2), "ClockCycles(clk,2)").await;
            s(&pv);
            pv.wait(clk.changed(), "ValueChange(clk)").await;
            s(&pv);
            pv.wait(cycles(clk, 2), "ClockCycles(clk,2)").await;
            s(&pv);
        }),
        spawn("edge_task", async move {
            for _ in 0..5 {
                pe.wait(clk.rising(), "RisingEdge(clk)").await;
                s(&pe);
            }
        }),
        spawn("cnt_task", async move {
            for _ in 0..2 {
                pc.wait(count.changed(), "ValueChange(count)").await;
                s(&pc);
                pc.wait(cycles(clk, 1), "ClockCycles(clk,1)").await;
                s(&pc);
            }
        }),
        spawn("z_task", async move {
            pz.wait(ev.wait(), "Event.wait()").await;
            s(&pz);
        }),
    ];
    for t in ts {
        t.await;
    }
    log.check("cycles_join");
}

#[simjig::test]
async fn rise_changed(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let (mut px, mut py, mut pw) = (
        log.probe("x_task"),
        log.probe("y_task"),
        log.probe("w_task"),
    );
    let s = seen(dut);
    let clk = dut.clk;
    counter_start(&mut p, dut).await;
    let ts = [
        spawn("x_task", async move {
            for k in [0, 1, 1, 0, 1, 2, 0, 1] {
                match k {
                    0 => px.wait(clk.rising(), "RisingEdge(clk)").await,
                    1 => px.wait(clk.changed(), "ValueChange(clk)").await,
                    _ => px.wait(clk.falling(), "FallingEdge(clk)").await,
                }
                s(&px);
            }
        }),
        spawn("y_task", async move {
            for _ in 0..8 {
                py.wait(clk.changed(), "ValueChange(clk)").await;
                s(&py);
            }
        }),
        spawn("w_task", async move {
            for _ in 0..4 {
                pw.wait(clk.rising(), "RisingEdge(clk)").await;
                s(&pw);
            }
        }),
    ];
    for t in ts {
        t.await;
    }
    log.check("rise_changed");
}

#[simjig::test]
async fn many_kinds(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let mut pz = log.probe("z_task");
    let ev = Event::new("ev");
    let (clk, count) = (dut.clk, dut.count);
    counter_start(&mut p, dut).await;
    let mut ts = Vec::new();
    for i in 0..24usize {
        let name: &'static str = Box::leak(format!("t{i:02}").into_boxed_str());
        let mut pi = log.probe(name);
        let set = ev.clone();
        ts.push(spawn(name, async move {
            for j in 0..3 {
                match (i + j) % 3 {
                    0 => pi.wait(clk.rising(), "RisingEdge(clk)").await,
                    1 => pi.wait(clk.changed(), "ValueChange(clk)").await,
                    _ => pi.wait(clk.falling(), "FallingEdge(clk)").await,
                }
                pi.read(&[&count]);
                if i == 4 && j == 0 {
                    set.set();
                }
            }
        }));
    }
    ts.push(spawn("z_task", async move {
        pz.wait(ev.wait(), "Event.wait()").await;
        pz.read(&[&count]);
    }));
    for t in ts {
        t.await;
    }
    log.check("many_kinds");
}

#[simjig::test]
async fn primed_walk(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let (mut pa, mut pb, mut pc, mut pd) = (
        log.probe("a_task"),
        log.probe("b_task"),
        log.probe("c_task"),
        log.probe("d_task"),
    );
    let s = seen(dut);
    let (clk, count) = (dut.clk, dut.count);
    counter_start(&mut p, dut).await;
    let ts = [
        spawn("a_task", async move {
            pa.wait(clk.rising(), "RisingEdge(clk)").await;
            s(&pa);
            pa.wait(count.changed(), "ValueChange(count)").await;
            s(&pa);
            pa.wait(clk.changed(), "ValueChange(clk)").await;
            s(&pa);
        }),
        spawn("b_task", async move {
            pb.wait(clk.changed(), "ValueChange(clk)").await;
            s(&pb);
            pb.wait(count.changed(), "ValueChange(count)").await;
            s(&pb);
            pb.wait(clk.rising(), "RisingEdge(clk)").await;
            s(&pb);
        }),
        spawn("c_task", async move {
            pc.wait(clk.rising(), "RisingEdge(clk)").await;
            s(&pc);
            pc.wait(clk.rising(), "RisingEdge(clk)").await;
            s(&pc);
            pc.wait(clk.changed(), "ValueChange(clk)").await;
            s(&pc);
        }),
        spawn("d_task", async move {
            pd.wait(count.changed(), "ValueChange(count)").await;
            s(&pd);
            pd.wait(clk.changed(), "ValueChange(clk)").await;
            s(&pd);
            pd.wait(count.changed(), "ValueChange(count)").await;
            s(&pd);
        }),
    ];
    for t in ts {
        t.await;
    }
    log.check("primed_walk");
}

#[simjig::test]
async fn spawned_combinators(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let clk = dut.clk;
    counter_start(&mut p, dut).await;
    let ev = Event::new("ev");
    let (mut pt, mut pa, mut pb, mut pc, mut pd, mut pe) = (
        log.probe("t_task"),
        log.probe("a_first"),
        log.probe("b_combine"),
        log.probe("c_timeout"),
        log.probe("d_plain"),
        log.probe("e_first2"),
    );
    let (e1, e2, e3, e4, e5, e6) = (
        ev.clone(),
        ev.clone(),
        ev.clone(),
        ev.clone(),
        ev.clone(),
        ev,
    );
    let mut ts = vec![
        spawn("t_task", async move {
            pt.wait(clk.rising(), "RisingEdge(clk)").await;
            s(&pt);
            e1.set();
        }),
        spawn("a_first", async move {
            pa.wait(async { first!(e2.wait(), Timer::new(ns(100))) }, "First")
                .await;
            s(&pa);
        }),
        spawn("b_combine", async move {
            pb.wait(
                async {
                    join!(e3.wait(), clk.rising());
                },
                "Combine",
            )
            .await;
            s(&pb);
        }),
        spawn("c_timeout", async move {
            pc.wait(with_timeout(ns(100), e4.wait()), "with_timeout")
                .await
                .unwrap();
            s(&pc);
        }),
        spawn("d_plain", async move {
            pd.wait(e5.wait(), "Event.wait()").await;
            s(&pd);
        }),
        spawn("e_first2", async move {
            pe.wait(async { first!(clk.rising(), e6.wait()) }, "First")
                .await;
            s(&pe);
        }),
    ];
    p.wait(Timer::new(ns(1)), "Timer(1ns)").await;
    let (tz, tf) = observers(dut, &log);
    ts.push(tz);
    ts.push(tf);
    for t in ts {
        t.await;
    }
    log.check("spawned_combinators");
}

/// Awaits `future` (dropping its output, a lock guard included) and returns `name`.
async fn named<T>(future: impl Future<Output = T>, name: &'static str) -> &'static str {
    drop(future.await);
    name
}

#[simjig::test]
async fn sync_losers(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let (mut pe, mut ph, mut pw) = (
        log.probe("e_task"),
        log.probe("holder"),
        log.probe("w_task"),
    );
    let s = seen(dut);
    let clk = dut.clk;
    let ev = Event::new("ev");
    let lock = Lock::new("lock");
    counter_start(&mut p, dut).await;

    // Event (15 ns): the edge wins; setting the Event afterwards wakes only e_task.
    let wait = ev.clone();
    let te = spawn("e_task", async move {
        pe.wait(wait.wait(), "Event.wait()").await;
        s(&pe);
    });
    let held = lock.clone();
    let th = spawn("holder", async move {
        let guard = held.lock().await; // 10 ns
        ph.wait(Timer::new(ns(17)), "Timer(17ns)").await; // 27 ns
        s(&ph);
        drop(guard);
    });
    let won = p
        .wait(
            async { first!(named(ev.wait(), "ev"), named(clk.rising(), "rise")) },
            "First",
        )
        .await;
    p.note("first", won);
    s(&p);
    ev.set();

    // Lock (25 ns): holder has it until 27 ns; main's lock(), first in line, loses to the
    // edge, so the lock goes to w_task.
    let wanted = lock.clone();
    let tw = spawn("w_task", async move {
        let guard = pw.wait(wanted.lock(), "Lock.acquire()").await; // 27 ns
        s(&pw);
        drop(guard);
    });
    let won = p
        .wait(
            async { first!(named(lock.lock(), "lock"), named(clk.rising(), "rise")) },
            "First",
        )
        .await;
    p.note("first", won);
    s(&p);
    th.await;
    tw.await;

    queue_round(&log, &mut p, clk, s).await;
    te.await;
    log.check("sync_losers");
}

/// `sync_losers` from 35 ns: main's `get()`, first in line, loses to the edge; the item put
/// at 37 ns goes to `g_task`.
async fn queue_round(
    log: &Log,
    p: &mut Probe,
    clk: simjig::In<simjig_bits::Bit>,
    s: impl Fn(&Probe) + Copy + 'static,
) {
    let mut pg = log.probe("g_task");
    let q: Queue<u32> = Queue::new("q", 4);
    let from = q.clone();
    let tg = spawn("g_task", async move {
        let item = pg.wait(from.get(), "Queue.get()").await; // 37 ns
        pg.note("item", &item.to_string());
        s(&pg);
    });
    let won = p
        .wait(
            async { first!(named(clk.rising(), "rise"), named(q.get(), "get")) },
            "First",
        )
        .await;
    p.note("first", won);
    s(p);
    p.wait(Timer::new(ns(2)), "Timer(2ns)").await; // 37 ns
    q.try_put(7).unwrap();
    tg.await;
}

#[simjig::test]
async fn cancelled_nest(dut: Dut) {
    let log = Log::default();
    let mut p = log.probe("main");
    let s = seen(dut);
    let (clk, count) = (dut.clk, dut.count);
    counter_start(&mut p, dut).await;
    let events: Vec<Event> = (0..6).map(|_| Event::new("ev")).collect();
    let mut pf = log.probe("f_task");
    let first_event = events[0].clone();
    let mut ts = vec![spawn("f_task", async move {
        pf.wait(clk.rising(), "RisingEdge(clk)").await; // 15 ns
        s(&pf);
        first_event.set();
    })];
    for (i, name) in ["z1", "z2", "z3", "z4", "z5"].into_iter().enumerate() {
        let mut pz = log.probe(name);
        let (wait, next) = (events[i].clone(), events[i + 1].clone());
        ts.push(spawn(name, async move {
            pz.wait(wait.wait(), "Event.wait()").await;
            s(&pz);
            next.set();
        }));
    }
    p.wait(Timer::new(ns(1)), "Timer(1ns)").await; // 11 ns
    p.wait(
        async {
            first!(
                async {
                    join!(count.changed(), Timer::new(ns(100)));
                },
                clk.rising()
            );
        },
        "First",
    )
    .await; // 15 ns
    s(&p);
    for t in ts {
        t.await;
    }
    log.check("cancelled_nest");
}
