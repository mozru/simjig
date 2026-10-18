//! The testbench library: what every test file of this package shares. Each
//! `tests/*.rs` starts with `use quickstart::*;`, which brings the simjig prelude, the
//! DUT modules (`inv::Dut`, `flop::Dut`) and [`reset`].

pub use simjig::prelude::*;

// The bindings `build.rs` generated: a module per design, with `Dut`, one typed handle per
// port.
simjig::include_dut!(inv);
simjig::include_dut!(flop);

/// Holds the flop in reset for one rising edge of its clock, and releases it right after
/// that edge.
pub async fn reset(dut: &flop::Dut) {
    dut.rst.set(1);
    dut.clk.rising().read_write().await;
    dut.rst.set(0);
}
