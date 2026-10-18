//! A clocked design: a kernel-driven clock, rising edges, and `ReadOnly` to see the values
//! a clock edge produced.
#![forbid(unsafe_code)]

use simjig::{ReadOnly, clock, ns};
use simjig_bits::Bit;

simjig::include_dut!(flop);

#[simjig::test(timeout = "1us")]
async fn q_follows_d(dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    for d in [true, false, true] {
        dut.d.set(d);
        dut.clk.rising().await;
        // At the edge the flop has not updated yet; its new value is visible in ReadOnly.
        ReadOnly::new().await;
        assert_eq!(dut.q.get(), Bit::from(d));
        dut.clk.falling().await;
    }
}
