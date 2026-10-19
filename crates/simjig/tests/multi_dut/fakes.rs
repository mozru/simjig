//! A hand-written Rust model next to a Verilator one: a reference inverter on a
//! `FakeModel`, compared in lockstep with the `inv` RTL. Also the errors of DUT sets that
//! cannot share a simulation.

use simjig::{
    DutModel, Failure, FakeModel, LoadedModel, Lockstep, ModelOptions, Precision, RawSig, Seed,
    SeedSource, Sim, ns, read_only, timer,
};
use simjig_bits::Bit;

use crate::inv;

/// A Rust inverter: `y = !a`, at the precision `P` (as a power of ten of seconds).
#[derive(Clone, Copy, Debug)]
struct RefInv<const P: i8> {
    a: RawSig,
    y: RawSig,
}

impl<const P: i8> DutModel for RefInv<P> {
    fn instantiate(options: &ModelOptions) -> Result<Sim, Failure> {
        Self::load(options).map(|_| unreachable!("only used next to another DUT"))
    }

    fn create(sim: &mut Sim) -> Result<Self, Failure> {
        Self::bind(sim, 0)
    }

    fn load(_options: &ModelOptions) -> Result<LoadedModel, Failure> {
        let mut m = FakeModel::new(Precision::from_exponent(P).unwrap());
        let a = m.signal("ref.a", 1);
        let y = m.signal("ref.y", 1);
        m.comb(move |s| s.set(y, u64::from(s.get(a) == 0)));
        Ok(LoadedModel::from_fake(m))
    }

    fn bind(sim: &mut Sim, model: usize) -> Result<Self, Failure> {
        let a = sim.signal_in(model, "ref.a").unwrap();
        let y = sim.signal_in(model, "ref.y").unwrap();
        Ok(Self { a, y })
    }
}

/// No `load`: the trait's default refuses to share a simulation.
#[derive(Debug)]
struct Alone;

impl DutModel for Alone {
    fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
        Ok(Sim::from_fake(FakeModel::new(Precision::PS)))
    }

    fn create(_sim: &mut Sim) -> Result<Self, Failure> {
        Ok(Self)
    }
}

#[simjig::test(timeout = "1us")]
async fn a_rust_model_runs_in_lockstep_with_the_rtl(rtl: inv::Dut, model: RefInv<-12>) {
    let mut lockstep = Lockstep::new().compare(
        "y",
        move || rtl.y.get(),
        move || Bit::from(model.y.read_u64() == 1),
    );
    for a in [false, true, true, false] {
        rtl.a.set(a);
        model.a.write_u64(u64::from(a));
        read_only().await;
        lockstep.step();
        timer(ns(1)).await;
    }
    assert_eq!(lockstep.cycle(), 4);
}

fn options() -> ModelOptions {
    ModelOptions::new(Seed::new(1, SeedSource::Env))
}

#[test]
fn a_dut_without_load_cannot_share_a_simulation() {
    let failure = <(inv::Dut, Alone)>::instantiate(&options()).unwrap_err();
    assert!(
        failure
            .message
            .contains("does not implement `DutModel::load`"),
        "{failure}"
    );
    assert!(failure.message.contains("Alone"), "{failure}");
    assert!(
        !failure.message.contains("implement `load`"),
        "the advice is the help's alone: {failure}"
    );
}

#[test]
fn models_must_share_a_precision() {
    let failure = <(inv::Dut, RefInv<-9>)>::instantiate(&options()).unwrap_err();
    assert!(
        failure
            .message
            .contains("model 1 has a precision of 1ns but model 0 has 1ps"),
        "{failure}"
    );
}
