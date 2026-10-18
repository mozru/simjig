//! (d) Triggers on `derived_clk`'s divided clock, an output the design computes from `clk`
//! in the same eval as the `clk`-domain flops (DECISION.md §1: a derived clock is sampled).
//! Every rising, falling and change of `div_clk` wakes the testbench exactly once, at the
//! `clk` edge that made it, and the slow domain has counted that edge by then.

use simjig::{ReadOnly, SimTime, clock, now, ns};
use simjig_bits::{Bit, Bits};

use crate::derived_clk::Dut;

async fn out_of_reset(dut: Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
}

#[simjig::test(timeout = "10us")]
async fn rising_edges_of_the_divided_clock_wake_once_each(dut: Dut) {
    out_of_reset(dut).await;
    let mut last: Option<SimTime> = None;
    for edge in 1..=20u32 {
        dut.div_clk.rising().await;
        let t: SimTime = now();
        // Every second posedge clk (10 ns clock: posedges at 0, 10, 20 ns, ...).
        if let Some(last) = last {
            assert_eq!(t.ticks() - last.ticks(), ns(20).ticks(), "edge {edge}");
        }
        last = Some(t);
        ReadOnly::new().await;
        assert_eq!(dut.div_clk.get(), Bit::ONE);
        assert_eq!(dut.slow_count.get(), Bits::new(u128::from(edge)));
        assert_eq!(
            dut.fast_count.get(),
            Bits::new(u128::from(2 * edge - 1)),
            "the clk domain counted the same edge"
        );
    }
}

#[simjig::test(timeout = "10us")]
async fn falling_and_changed_follow_the_divided_clock(dut: Dut) {
    out_of_reset(dut).await;
    dut.div_clk.rising().await;
    let rose = now();
    dut.div_clk.falling().await;
    assert_eq!(now().ticks() - rose.ticks(), ns(10).ticks());
    dut.div_clk.changed().await;
    assert_eq!(now().ticks() - rose.ticks(), ns(20).ticks());
    ReadOnly::new().await;
    assert_eq!(dut.div_clk.get(), Bit::ONE);
}
