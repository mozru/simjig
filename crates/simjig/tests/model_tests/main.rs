//! The Verilator backend and the typed handles, against real Verilator models of the shared
//! DUTs (`tests/duts`), built by `build.rs` through simjig-build.
//!
//! The bindings are the generated ones (`include_dut!` below), so these tests also pin the
//! contract between generated code and `simjig::__private`.
#![expect(clippy::unwrap_used, reason = "test helpers outside #[test] functions")]

#[cfg(not(simjig_test_models))]
#[test]
fn models_were_built() {
    panic!(
        "build.rs could not build the test models: {}",
        env!("SIMJIG_TEST_MODELS_ERROR")
    );
}

#[cfg(simjig_test_models)]
simjig::include_dut!(inv);
#[cfg(simjig_test_models)]
simjig::include_dut!(flop);
#[cfg(simjig_test_models)]
simjig::include_dut!(counter);
#[cfg(simjig_test_models)]
simjig::include_dut!(fatal);
#[cfg(simjig_test_models)]
simjig::include_dut!(wide);
#[cfg(simjig_test_models)]
simjig::include_dut!(timing_dut);
#[cfg(simjig_test_models)]
simjig::include_dut!(hello);
#[cfg(simjig_test_models)]
simjig::include_dut!(rw_time);
#[cfg(simjig_test_models)]
simjig::include_dut!(rw_meta);
#[cfg(simjig_test_models)]
simjig::include_dut!(writes_input);
#[cfg(simjig_test_models)]
simjig::include_dut!(input_time);
#[cfg(simjig_test_models)]
simjig::include_dut!(no_reset);
#[cfg(simjig_test_models)]
simjig::include_dut!(no_reset_fast);

#[cfg(simjig_test_models)]
mod bench;
#[cfg(simjig_test_models)]
mod codegen;
#[cfg(simjig_test_models)]
mod duts;
#[cfg(simjig_test_models)]
mod elision;
#[cfg(simjig_test_models)]
mod handles;
#[cfg(simjig_test_models)]
mod init;
#[cfg(simjig_test_models)]
mod messages;
#[cfg(simjig_test_models)]
mod sync;
#[cfg(simjig_test_models)]
mod walks;

#[cfg(simjig_test_models)]
use simjig::{Clock, Precision, RawSig, Sim, SimDuration, TimeUnit};

/// A 10 ns clock on `clk`, starting low and rising first at 5 ns (cocotb's
/// `Clock(clk, 10, "ns").start(start_high=False)`).
#[cfg(simjig_test_models)]
pub(crate) fn start_clock(sim: &mut Sim, clk: RawSig) {
    let p: Precision = sim.precision();
    let clock = Clock::new(clk, p.duration(10, TimeUnit::Ns).unwrap())
        .first_rise_at(p.duration(5, TimeUnit::Ns).unwrap());
    sim.add_clock(clock).unwrap();
}

/// `n` ticks of the design's precision.
#[cfg(simjig_test_models)]
pub(crate) const fn ticks(n: u64) -> SimDuration {
    SimDuration::from_ticks(n)
}
