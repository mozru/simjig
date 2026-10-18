//! Random initial values (`InitValues::Random`) against a real model with a register that
//! reset never clears (`no_reset.sv`).

use simjig::fork;
use simjig::{
    DutModel, FailureKind, InitValues, ModelOptions, ReadOnly, Seed, SeedSource, TestCtx,
};
use simjig_bits::Bits;

use crate::{no_reset, no_reset_fast};

/// `q`, `r` and `u` of a fresh model, read at time 0 before any clock edge.
type Start = (Bits<32>, Bits<32>, Bits<32>);

fn options(seed: u64, init: InitValues) -> ModelOptions {
    ModelOptions::new(Seed::new(seed, SeedSource::Env)).with_init(init)
}

fn start(options: &ModelOptions) -> Start {
    let mut sim = no_reset::Dut::instantiate(options).unwrap();
    let dut = no_reset::Dut::create(&mut sim).unwrap();
    sim.run(async move {
        ReadOnly::new().await;
        (dut.q.get(), dut.r.get(), dut.u.get())
    })
    .unwrap()
}

#[test]
fn a_register_without_reset_starts_at_zero_by_default() {
    let (q, r, _) = start(&options(1, InitValues::Zero));
    assert_eq!((q, r), (Bits::new(0), Bits::new(0)));
}

#[test]
fn random_initial_values_follow_the_seed() {
    let starts: Vec<Start> = (1..=4)
        .map(|seed| start(&options(seed, InitValues::Random)))
        .collect();
    for (i, a) in starts.iter().enumerate() {
        assert_ne!(a.0, Bits::new(0), "seed {}: q is random", i + 1);
        for b in &starts[i + 1..] {
            assert_ne!(a.0, b.0, "different seeds start apart");
        }
    }
    assert_eq!(
        start(&options(3, InitValues::Random)),
        starts[2],
        "the same seed, the same start"
    );
}

/// Only the start moves: `$urandom` (here one draw at time 0) is the zero run's.
#[test]
fn random_initial_values_leave_urandom_alone() {
    let zero = start(&options(7, InitValues::Zero));
    let random = start(&options(7, InitValues::Random));
    assert_ne!(random.0, zero.0);
    assert_eq!(random.2, zero.2);
}

#[test]
fn a_model_with_compiled_in_initial_values_refuses_random_ones() {
    let failure = no_reset_fast::Dut::instantiate(&options(1, InitValues::Random)).unwrap_err();
    assert!(
        matches!(failure.kind, FailureKind::Setup { .. }),
        "{failure}"
    );
    let text = failure.to_string();
    assert!(
        text.contains("cannot start from random initial values"),
        "{text}"
    );
    assert!(text.contains("XInitial::Unique"), "{text}");
    assert!(text.contains("simjig-build older than random"), "{text}");
    assert!(no_reset_fast::Dut::instantiate(&options(1, InitValues::Zero)).is_ok());
}

/// Two instances of one design share the `$urandom` seed and the initial-value seed
/// (`harness::multi`), so they start alike and a `Lockstep` of them does not diverge at
/// cycle 0; model 0 keeps the seed it has alone.
#[test]
fn the_models_of_one_design_start_alike() {
    type Pair = (no_reset::Dut, no_reset::Dut);
    let run = |init| {
        let mut sim = Pair::instantiate(&options(5, init)).unwrap();
        let (a, b) = Pair::create(&mut sim).unwrap();
        sim.run(async move {
            ReadOnly::new().await;
            let diverged = simjig::Lockstep::new()
                .output(a.q, b.q)
                .output(a.r, b.r)
                .check()
                .err();
            ((a.q.get(), a.u.get()), (b.q.get(), b.u.get()), diverged)
        })
        .unwrap()
    };
    let ((qa, ua), (qb, ub), diverged) = run(InitValues::Random);
    assert!(diverged.is_none(), "{diverged:?}");
    assert_ne!(qa, Bits::new(0));
    assert_eq!((qa, ua), (qb, ub));
    assert_eq!(
        qa,
        start(&options(5, InitValues::Random)).0,
        "model 0 keeps stream 0"
    );
    let ((qa, _), (qb, _), _) = run(InitValues::Zero);
    assert_eq!((qa, qb), (Bits::new(0), Bits::new(0)));
}

/// The attribute end to end: a test-name seed, and fork branches inherit the start
/// rather than drawing again.
#[simjig::test(init = random, timeout = "1us")]
async fn the_attribute_randomizes_and_branches_inherit_it(ctx: &TestCtx, dut: no_reset::Dut) {
    ReadOnly::new().await;
    let q = dut.q.get();
    assert_ne!(q, Bits::new(0));
    let branches = fork::run_from_checkpoint(ctx.seed(), 2, |_| async move { Ok(dut.q.get()) })
        .await
        .unwrap();
    for outcome in branches.outcomes() {
        assert_eq!(outcome.result.as_ref().ok(), Some(&q));
    }
}
