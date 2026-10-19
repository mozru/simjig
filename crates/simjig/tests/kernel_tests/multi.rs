//! Several models in one simulation: shared time, one delta loop, one flat slot space.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::{
    Clock, FailureKind, FakeModel, ModelSetError, Precision, Sim, SimTime, TraceKind, changed,
    cycles, now, read_only, rising, spawn, timer, yield_now,
};

use super::{assert_clean, ticks};

/// A counter: `count` increments on every rising edge of `clk`.
fn counter() -> FakeModel {
    let mut m = FakeModel::new(Precision::NS);
    let clk = m.signal("clk", 1);
    let count = m.signal("count", 8);
    m.on_rising(clk, move |s| s.set(count, s.get(count) + 1));
    m
}

#[test]
fn same_named_signals_of_two_models_are_distinct() {
    let mut sim = Sim::from_fakes([counter(), counter()]).unwrap();
    assert_eq!(sim.model_count(), 2);
    let (clk0, n0) = (sim.signal("clk").unwrap(), sim.signal("count").unwrap());
    let (clk1, n1) = (
        sim.signal_in(1, "clk").unwrap(),
        sim.signal_in(1, "count").unwrap(),
    );
    assert_ne!(clk0, clk1);
    assert_eq!((clk0.model(), clk1.model()), (0, 1));
    assert_eq!(sim.signal_in(2, "clk"), None);
    sim.add_clock(Clock::new(clk0, ticks(10))).unwrap();
    sim.add_clock(Clock::new(clk1, ticks(4))).unwrap();
    let counts = sim
        .run(async move {
            timer(ticks(41)).await;
            (n0.read_u64(), n1.read_u64(), clk1.name())
        })
        .unwrap();
    // Edges at 0..=40: every 10 ticks for model 0, every 4 for model 1.
    assert_eq!(counts, (5, 11, "clk".to_owned()));
    assert_clean(&sim);
}

#[test]
fn every_delta_evaluates_all_models_in_declaration_order() {
    let log: Rc<RefCell<Vec<char>>> = Rc::default();
    let model = |tag: char| {
        let mut m = FakeModel::new(Precision::NS);
        m.signal("x", 8);
        let log = Rc::clone(&log);
        m.comb(move |_| log.borrow_mut().push(tag));
        m
    };
    let mut sim = Sim::from_fakes([model('a'), model('b')]).unwrap();
    let xb = sim.signal_in(1, "x").unwrap();
    sim.run(async move {
        // Two evals at t=0 (the write to b's input is applied after the first, so a
        // second runs), then one at t=1.
        xb.write_u64(1);
        timer(ticks(1)).await;
        read_only().await;
    })
    .unwrap();
    let evals = sim
        .recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Eval)
        .count();
    assert_eq!(evals, 3);
    // One settle pass per model per eval (nothing combinational changes).
    assert_eq!(*log.borrow(), ['a', 'b', 'a', 'b', 'a', 'b']);
}

#[test]
fn edge_waiters_across_models_wake_in_arm_order() {
    let mut sim = Sim::from_fakes([counter(), counter()]).unwrap();
    let (clk0, n0) = (sim.signal("clk").unwrap(), sim.signal("count").unwrap());
    let (clk1, n1) = (
        sim.signal_in(1, "clk").unwrap(),
        sim.signal_in(1, "count").unwrap(),
    );
    sim.add_clock(Clock::new(clk0, ticks(10))).unwrap();
    sim.add_clock(Clock::new(clk1, ticks(10))).unwrap();
    let order: Rc<RefCell<Vec<&str>>> = Rc::default();
    let o = Rc::clone(&order);
    sim.run(async move {
        rising(clk0).await;
        // Both counters change in the eval after this edge; model 1's waiter is armed
        // first, so it wakes first although model 0 is compared first.
        let o1 = Rc::clone(&o);
        let first = spawn("on_model1", async move {
            changed(n1).await;
            o1.borrow_mut().push("model 1");
        });
        yield_now().await;
        changed(n0).await;
        o.borrow_mut().push("model 0");
        first.await;
        cycles(clk1, 1).await;
    })
    .unwrap();
    assert_eq!(*order.borrow(), ["model 1", "model 0"]);
}

#[test]
fn time_advances_to_the_earliest_event_of_any_model() {
    let timed = |times: &'static [u64]| {
        let mut m = FakeModel::new(Precision::NS);
        let x = m.signal("x", 8);
        for &t in times {
            m.at(SimTime(t), move |s| s.set(x, t));
        }
        m
    };
    let mut sim = Sim::from_fakes([timed(&[7, 30]), timed(&[12, 30, 33])]).unwrap();
    let x1 = sim.signal_in(1, "x").unwrap();
    let seen = sim
        .run(async move {
            changed(x1).await; // 12
            let at_12 = (now(), x1.read_u64());
            timer(ticks(13)).await; // 25
            changed(x1).await; // 30
            changed(x1).await; // 33
            (at_12, now(), x1.read_u64())
        })
        .unwrap();
    assert_eq!(seen, ((SimTime(12), 12), SimTime(33), 33));
    let steps: Vec<u64> = sim
        .recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect();
    assert_eq!(steps, [0, 7, 12, 25, 30, 33]);
}

#[test]
fn model_failures_name_the_model() {
    for finish in [false, true] {
        let mut b = FakeModel::new(Precision::NS);
        let go = b.signal("go", 1);
        b.comb(move |s| {
            if s.get(go) == 1 {
                if finish {
                    s.finish();
                } else {
                    s.fatal("assertion failed in top.chk");
                }
            }
        });
        let mut sim = Sim::from_fakes([counter(), b]).unwrap();
        let go = sim.signal_in(1, "go").unwrap();
        let f = sim
            .run(async move {
                go.write_u64(1);
                timer(ticks(5)).await;
            })
            .unwrap_err();
        if finish {
            assert_eq!(f.kind, FailureKind::DesignFinished);
            assert!(
                f.message
                    .starts_with("model 1: the design executed $finish")
            );
        } else {
            let message = "model 1: assertion failed in top.chk".to_owned();
            assert_eq!(f.kind, FailureKind::ModelFatal { message });
        }
        assert_clean(&sim);
    }
}

#[test]
fn models_must_share_a_precision() {
    let err = Sim::from_fakes([counter(), FakeModel::new(Precision::PS)]).unwrap_err();
    assert_eq!(
        err,
        ModelSetError::PrecisionMismatch {
            model: 1,
            precision: Precision::PS,
            expected: Precision::NS,
        }
    );
    assert!(
        err.to_string().contains("must share a timeprecision"),
        "{err}"
    );
    assert_eq!(
        Sim::from_fakes(Vec::new()).unwrap_err(),
        ModelSetError::Empty
    );
}
