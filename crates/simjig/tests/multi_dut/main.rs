//! Several DUTs in one `#[simjig::test]`: two different designs, the same design twice, a
//! Rust model next to a Verilator one, and lockstep comparison of two RTL revisions of
//! `counter` (`counter_rev2.sv` skips 6). Every model is a real Verilator build from
//! `build.rs`, except the hand-written `FakeModel` DUTs.
#![expect(clippy::unwrap_used, reason = "a panic is the test failure")]

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
simjig::include_dut!(counter_rev2);
#[cfg(simjig_test_models)]
simjig::include_dut!(fatal);

#[cfg(simjig_test_models)]
mod designs;
#[cfg(simjig_test_models)]
mod fakes;
#[cfg(simjig_test_models)]
mod lockstep;
