//! The Verilator backend and the typed handles, against real Verilator models of the shared
//! DUTs (`tests/duts`), compiled by `build.rs` with the hand-written shim in `shim.cpp`.
//!
//! `duts.rs` is written exactly as simjig-build is to generate it, so these tests also pin
//! the contract between generated code and `simjig::__private`.
#![expect(clippy::unwrap_used, reason = "test helpers outside #[test] functions")]

#[cfg(not(simjig_abi_models))]
#[test]
fn models_were_built() {
    panic!(
        "build.rs could not build the test models: {}",
        env!("SIMJIG_ABI_MODELS_ERROR")
    );
}

#[cfg(simjig_abi_models)]
mod bench;
#[cfg(simjig_abi_models)]
mod codegen;
#[cfg(simjig_abi_models)]
mod conflog;
#[cfg(simjig_abi_models)]
mod conformance;
#[cfg(simjig_abi_models)]
mod duts;
#[cfg(simjig_abi_models)]
mod handles;
#[cfg(simjig_abi_models)]
mod messages;

#[cfg(simjig_abi_models)]
use simjig::{Clock, Precision, RawSig, Sim, SimDuration, TimeUnit};

/// A 10 ns clock on `clk`, starting low and rising first at 5 ns (cocotb's
/// `Clock(clk, 10, "ns").start(start_high=False)`).
#[cfg(simjig_abi_models)]
pub(crate) fn start_clock(sim: &mut Sim, clk: RawSig) {
    let p: Precision = sim.precision();
    let clock = Clock::new(clk, p.duration(10, TimeUnit::Ns).unwrap())
        .first_rise_at(p.duration(5, TimeUnit::Ns).unwrap());
    sim.add_clock(clock).unwrap();
}

/// `n` ticks of the design's precision.
#[cfg(simjig_abi_models)]
pub(crate) const fn ticks(n: u64) -> SimDuration {
    SimDuration::from_ticks(n)
}
