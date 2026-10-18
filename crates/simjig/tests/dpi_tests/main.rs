//! DPI-C imports implemented in Rust, end to end: `tests/duts/dpi` and `dpi_types.sv` built
//! by `build.rs` through the real pipeline (JSON pass, generated trait and trampolines,
//! `__Dpi.h` cross-check, verilation, compilation), driven through `Sim` and
//! `#[simjig::test]`.
//!
//! The crate forbids `unsafe_code`: the generated trampolines (`#[no_mangle]`, pointer
//! reads) must compile in a user crate that does.
#![forbid(unsafe_code)]
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
simjig::include_dut!(dpi);
#[cfg(simjig_test_models)]
simjig::include_dut!(dpi_types);

#[cfg(simjig_test_models)]
mod basics;
#[cfg(simjig_test_models)]
mod bench;
#[cfg(simjig_test_models)]
mod failures;
#[cfg(simjig_test_models)]
mod types;

/// A loaded model and its `Dut`, as the harness makes them.
#[cfg(simjig_test_models)]
pub(crate) fn sim<D: simjig::DutModel>() -> (simjig::Sim, D) {
    let options = simjig::ModelOptions::new(simjig::Seed::new(1, simjig::SeedSource::Env));
    let mut sim = D::instantiate(&options).unwrap();
    let dut = D::create(&mut sim).unwrap();
    (sim, dut)
}

/// One tick of the design's precision.
#[cfg(simjig_test_models)]
pub(crate) const TICK: simjig::SimDuration = simjig::SimDuration::from_ticks(1);

/// Drives `clk` through `n` cycles of two ticks (rising edge first).
#[cfg(simjig_test_models)]
pub(crate) async fn cycles(clk: simjig::__private::In<simjig_bits::Bit>, n: u32) {
    for _ in 0..n {
        clk.set(true);
        simjig::Timer::new(TICK).await;
        clk.set(false);
        simjig::Timer::new(TICK).await;
    }
}
#[cfg(all(unix, simjig_test_models))]
mod s3c4m;
