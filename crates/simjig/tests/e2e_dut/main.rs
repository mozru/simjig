//! simjig end to end, written the way a user writes a testbench: each DUT is built by
//! `build.rs` through simjig-build, its bindings come from `include_dut!`, and every test is
//! a `#[simjig::test]` over the generated `Dut`. No type here is hand-written: the struct,
//! enum and bundle types are the generated ones.
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
simjig::include_dut!(struct_pipe);
#[cfg(simjig_test_models)]
simjig::include_dut!(fifo);
#[cfg(simjig_test_models)]
simjig::include_dut!(wide);
#[cfg(simjig_test_models)]
simjig::include_dut!(fatal);
#[cfg(simjig_test_models)]
simjig::include_dut!(iface_top);

#[cfg(simjig_test_models)]
mod fatal_tb;
#[cfg(simjig_test_models)]
mod fifo_tb;
#[cfg(simjig_test_models)]
mod flop_tb;
#[cfg(simjig_test_models)]
mod iface_tb;
#[cfg(simjig_test_models)]
mod inv_tb;
#[cfg(simjig_test_models)]
mod struct_pipe_tb;
#[cfg(simjig_test_models)]
mod wide_tb;
