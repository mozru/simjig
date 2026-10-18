//! Two DUTs in one test: they share one simulation (one time axis, one delta loop), and the
//! testbench connects them. Here the inverter's output feeds the flop's `d`.

use quickstart::*;

#[simjig::test(timeout = "1us")]
async fn the_flop_captures_the_inverted_input(inv: inv::Dut, flop: flop::Dut) {
    clock(flop.clk, ns(10)).start();
    reset(&flop).await;
    for a in [false, true, false] {
        inv.a.set(a);
        Timer::new(ns(1)).await;
        flop.d.set(inv.y.get());
        flop.clk.rising().read_write().await;
        assert_eq!(flop.q.get(), Bit::from(!a));
    }
}
