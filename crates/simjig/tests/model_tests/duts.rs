//! The test models: the bindings simjig-build generated (`include_dut!`d at the crate
//! root, as a user would, so the generated `pub` items are reachable), and the harness's
//! way of building one.

pub(crate) use crate::{counter, fatal, flop, hello, inv, timing_dut, wide};

/// A new simulation of `D` and its handles, as the harness builds them.
pub(crate) fn sim<D: simjig::DutModel>() -> (simjig::Sim, D) {
    sim_with(&simjig::ModelOptions::new(simjig::Seed::new(
        1,
        simjig::SeedSource::Env,
    )))
}

/// [`sim`] with these options.
pub(crate) fn sim_with<D: simjig::DutModel>(options: &simjig::ModelOptions) -> (simjig::Sim, D) {
    let mut sim = D::instantiate(options).unwrap();
    let dut = D::create(&mut sim).unwrap();
    (sim, dut)
}
