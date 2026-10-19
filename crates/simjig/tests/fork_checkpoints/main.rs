//! `simjig::fork`: branches from one checkpoint, on real Verilator models (`counter`,
//! `fatal`). These tests fork the test process, so they need nextest (one test per process);
//! under multi-threaded `cargo test` every fork is refused with `ForkError::Threads`.
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
simjig::include_dut!(counter);
#[cfg(simjig_test_models)]
simjig::include_dut!(fatal);

#[cfg(all(unix, simjig_test_models))]
mod branches;
#[cfg(all(unix, simjig_test_models))]
mod combinators;
#[cfg(all(unix, simjig_test_models))]
mod cost;
#[cfg(all(unix, simjig_test_models))]
mod failures;
#[cfg(all(unix, simjig_test_models))]
mod nested_combinators;
#[cfg(all(unix, simjig_test_models))]
mod rules;
