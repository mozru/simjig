//! FST waves through the real pipeline: models built by `build.rs` with `.trace_fst(true)`
//! (`counter_fst`, `fifo_fst`, from tests/duts), traced through `TraceOptions` and read back
//! with a small FST reader; and waves on failure, where the harness replays a failed test.
#![expect(clippy::expect_used, reason = "test helpers outside #[test] functions")]

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
simjig::include_dut!(counter_fst);
#[cfg(simjig_test_models)]
simjig::include_dut!(fifo_fst);

mod fst;
#[cfg(simjig_test_models)]
mod on_fail;
#[cfg(simjig_test_models)]
mod tracing;
