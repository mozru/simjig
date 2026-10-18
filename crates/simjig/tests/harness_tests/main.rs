//! Tests of the test harness (`#[simjig::test]`, seeds, failure reports, run directories),
//! all on FakeModel-backed DUTs (no Verilator needed).
#![expect(clippy::expect_used, reason = "test helpers outside #[test] functions")]

// The DUT bindings, included the way generated ones are (`include_dut!(counter)` reads
// `$OUT_DIR/simjig/counter.rs`); the `@from` form reads a hand-written file instead.
simjig::include_dut!(@from concat!(env!("CARGO_MANIFEST_DIR"), "/tests/harness_tests/duts"), counter);

mod macros;
mod reports;
mod repro;
mod runner;
mod ui;
