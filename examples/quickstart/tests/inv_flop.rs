//! Two DUTs in one test: they share one simulation (one time axis, one delta loop), and the
//! testbench connects them. Here the inverter's output feeds the flop's `d`.
#![forbid(unsafe_code)]

use simjig::{ReadOnly, Timer, clock, ns};
use simjig_bits::Bit;

simjig::include_dut!(inv);
simjig::include_dut!(flop);

#[simjig::test(timeout = "1us")]
async fn the_flop_captures_the_inverted_input(inv: inv::Dut, flop: flop::Dut) {
    clock(flop.clk, ns(10)).start();
    flop.rst.set(1);
    flop.clk.rising().await;
    flop.clk.falling().await;
    flop.rst.set(0);
    for a in [false, true, false] {
        inv.a.set(a);
        Timer::new(ns(1)).await;
        flop.d.set(inv.y.get());
        flop.clk.rising().await;
        ReadOnly::new().await;
        assert_eq!(flop.q.get(), Bit::from(!a));
        flop.clk.falling().await;
    }
}
