//! The testbench library of this crate's tests: each `tests/<module>.rs` starts with
//! `use simjig_examples_lib::*;`, which brings the simjig prelude, every DUT module the
//! build script made, the handle types `In` and `Out` that the tests' helpers take, and
//! [`assert_param_checks`].
//!
//! [`assert_param_checks`] runs Verilator's elaboration on a module with chosen
//! parameters, outside any model: each module's parameter checks (`$fatal` in a generate
//! block, IEEE 1800-2023 §20.11) stop elaboration, which Verilator 5.052 reports as a
//! `USERFATAL` warning that fails the run (warnings are fatal without `-Wno-fatal`).

use std::process::Command;

pub use simjig::prelude::*;
pub use simjig::{In, Out};

simjig::include_dut!(sync_fifo); // the sweep: sync_fifo::{Ports, fifo_8x4, fifo_32x16}
simjig::include_dut!(item_fifo);
simjig::include_dut!(skid_buffer);
simjig::include_dut!(round_robin_arbiter);
simjig::include_dut!(async_fifo);
simjig::include_dut!(hop_stage); // the sweep: hop_stage::{Ports, BeatFields, hop_8x2, hop_32x4}

/// What `verilator --lint-only -Wall` said about one parameter set.
#[derive(Debug)]
struct Lint {
    passed: bool,
    stderr: String,
}

/// Lints `top` from `sources` (paths relative to this crate's root) with `-G` overrides
/// `params` (such as `"DEPTH=6"`). Needs `verilator` on `PATH`: run under `pixi run`.
fn lint(top: &str, sources: &[&str], params: &[&str]) -> Lint {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("verilator")
        .args(["--lint-only", "-Wall", "--top-module", top])
        .args(sources.iter().map(|s| format!("{root}/{s}")))
        .args(params.iter().map(|p| format!("-G{p}")))
        .output()
        .expect("verilator on PATH (run under `pixi run`)");
    Lint {
        passed: out.status.success(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Asserts that every parameter set in `good` elaborates cleanly and every one in `bad`
/// stops with a message containing `message`.
///
/// # Panics
/// On the first parameter set that does otherwise.
#[track_caller]
pub fn assert_param_checks(
    top: &str,
    sources: &[&str],
    good: &[&[&str]],
    bad: &[&[&str]],
    message: &str,
) {
    for params in good {
        let lint = lint(top, sources, params);
        assert!(lint.passed, "{top} {params:?}: {}", lint.stderr);
    }
    for params in bad {
        let lint = lint(top, sources, params);
        assert!(!lint.passed, "{top} {params:?} elaborated");
        assert!(
            lint.stderr.contains(message) && lint.stderr.contains("USERFATAL"),
            "{top} {params:?}: want a $fatal with {message:?}: {}",
            lint.stderr
        );
    }
}
