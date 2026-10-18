//! Stage 3's exit tests: real Verilator models built by `build.rs`, driven only through
//! simjig's public API, one file per capability:
//! - `cdc`: `async_fifo` on two unrelated kernel clocks, data crossing intact (a);
//! - `timing`: `timing_dut` (`--timing`) with timers and a kernel clock interleaved (b);
//! - `multi`: two models in one test, and lockstep divergence (c);
//! - `derived`: triggers on `derived_clk`'s divided clock (d);
//! - `waves`: waves on failure for a failing test (e);
//! - `dpi`: DPI-C round trips, Rust and C implementations (f);
//! - `iface`: `iface_top` with probes behind the interface wrapper (g);
//! - `coverage`: coverage counters read from Rust (h).
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
simjig::include_dut!(counter_cov);

#[cfg(simjig_test_models)]
mod coverage;
