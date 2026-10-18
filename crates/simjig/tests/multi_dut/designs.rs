//! Two different designs, and one design twice, sharing one simulation.

use simjig::{DutModel, FailureKind, ModelOptions, ReadOnly, Seed, SeedSource, clock, ns, spawn};
use simjig_bits::{Bit, Bits};

use crate::{counter, fatal, flop, inv};

/// `inv.y` feeds `flop.d` through a forwarding task: each model is evaluated in every delta,
/// and the forwarded value lands one delta later, before the next clock edge.
#[simjig::test(timeout = "1us")]
async fn two_different_designs_share_one_delta_loop(i: inv::Dut, f: flop::Dut) {
    clock(f.clk, ns(10)).start();
    spawn("forward", async move {
        loop {
            f.d.set(i.y.get());
            i.y.changed().await;
        }
    })
    .detach();
    f.rst.set(0);
    for a in [false, true, true, false, true] {
        f.clk.falling().await;
        i.a.set(a);
        ReadOnly::new().await;
        assert_eq!(f.d.get(), Bit::from(!a), "inv.y reached flop.d");
        f.clk.rising().await;
        ReadOnly::new().await;
        assert_eq!(f.q.get(), Bit::from(!a), "flop captured ~a");
    }
}

/// Two instances of one build: separate state (each its own `VerilatedContext`), one clock
/// domain each, in one time axis.
#[simjig::test(timeout = "1us")]
async fn the_same_design_twice_keeps_separate_state(a: counter::Dut, b: counter::Dut) {
    clock(a.clk, ns(10)).start();
    clock(b.clk, ns(10)).start();
    for dut in [a, b] {
        dut.rst.set(1);
    }
    a.clk.rising().await;
    a.clk.falling().await;
    for dut in [a, b] {
        dut.rst.set(0);
    }
    a.en.set(1);
    b.en.set(0);
    for _ in 0..5 {
        a.clk.rising().await;
    }
    ReadOnly::new().await;
    assert_eq!(a.count.get(), Bits::<8>::new(5));
    assert_eq!(b.count.get(), Bits::<8>::new(0));
    assert_eq!(a.count.name(), b.count.name(), "both are `counter.count`");
}

/// A `$fatal` in model 0 is reported against model 0, although model 1 was constructed
/// last (and so was the thread's Verilator context): each backend switches to its own
/// context before running model code.
#[test]
fn a_fatal_names_the_model_that_raised_it() {
    let options = ModelOptions::new(Seed::new(1, SeedSource::Env));
    let mut sim = <(fatal::Dut, inv::Dut)>::instantiate(&options).unwrap();
    let (f, i) = <(fatal::Dut, inv::Dut)>::create(&mut sim).unwrap();
    assert_eq!(sim.model_count(), 2);
    let failure = sim
        .run(async move {
            clock(f.clk, ns(10)).start();
            f.rst.set(0);
            f.kind.set(0);
            f.fire.set(1);
            i.a.set(1);
            for _ in 0..4 {
                f.clk.rising().await;
            }
        })
        .unwrap_err();
    assert!(
        matches!(failure.kind, FailureKind::ModelFatal { .. }),
        "{failure}"
    );
    assert!(failure.message.contains("model 0"), "{failure}");
    assert!(failure.message.contains("fatal: kind=0"), "{failure}");
}

/// Handles bind to their own model: the inverter's input is model 1's slot, not model 0's
/// slot of the same number.
#[test]
fn handles_resolve_to_their_own_model() {
    let options = ModelOptions::new(Seed::new(1, SeedSource::Env));
    let mut sim = <(flop::Dut, inv::Dut)>::instantiate(&options).unwrap();
    let (f, i) = <(flop::Dut, inv::Dut)>::create(&mut sim).unwrap();
    assert_eq!(f.clk.raw().model(), 0);
    assert_eq!(i.a.raw().model(), 1);
    let y = sim
        .run(async move {
            i.a.set(0);
            ReadOnly::new().await;
            i.y.get()
        })
        .unwrap();
    assert_eq!(y, Bit::ONE);
}
