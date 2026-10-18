//! The 14 cocotb conformance scenarios (`tests/conformance`) replayed against real
//! Verilator models through the public API only: `include_dut!` bindings, `#[simjig::test]`
//! and `simjig::clock`. Each test mirrors its `tests/conformance/tb/test_<dut>.py` function
//! line by line and diffs its log byte for byte with `tests/conformance/golden/<name>.jsonl`
//! (cocotb 2.1.0 on Verilator 5.052).
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
simjig::include_dut!(flop);
#[cfg(simjig_test_models)]
simjig::include_dut!(counter);
#[cfg(simjig_test_models)]
simjig::include_dut!(comb_chain);
#[cfg(simjig_test_models)]
simjig::include_dut!(fifo);
#[cfg(simjig_test_models)]
simjig::include_dut!(async_fifo);
#[cfg(simjig_test_models)]
simjig::include_dut!(derived_clk);

#[cfg(simjig_test_models)]
mod conflog;
#[cfg(simjig_test_models)]
mod test_async_fifo;
#[cfg(simjig_test_models)]
mod test_comb_chain;
#[cfg(simjig_test_models)]
mod test_counter;
#[cfg(simjig_test_models)]
mod test_derived_clk;
#[cfg(simjig_test_models)]
mod test_fifo;
#[cfg(simjig_test_models)]
mod test_flop;
#[cfg(simjig_test_models)]
mod test_timeout_ties;

/// cocotb's `Clock(sig, period, "ns").start(start_high=False)`: low for the first half
/// period, so a 10 ns clock rises at 5, 15, 25, ... ns.
#[cfg(simjig_test_models)]
fn start_clock(clk: simjig::In<simjig_bits::Bit>, period_ns: u64) {
    simjig::clock(clk, simjig::ns(period_ns))
        .phase(simjig::ns(period_ns / 2))
        .start();
}
