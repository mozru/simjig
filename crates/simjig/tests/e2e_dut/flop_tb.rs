//! `flop`: a kernel clock, `rising`, and `ReadOnly` for post-edge values.

use simjig::{clock, ns, read_only};
use simjig_bits::Bit;

use crate::flop;

#[simjig::test(timeout = "1us")]
async fn q_takes_d_at_each_rising_edge(dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.d.set(0);
    dut.clk.rising().await;
    read_only().await;
    assert_eq!(dut.q.get(), Bit::ZERO, "reset");

    let mut q = Bit::ZERO;
    dut.clk.falling().await;
    dut.rst.set(0);
    for d in [true, false, true, true, false] {
        dut.d.set(d);
        dut.clk.rising().await;
        // A testbench-driven edge resumes before the eval that clocks the flop.
        assert_eq!(dut.q.get(), q, "pre-edge value at the rising edge");
        read_only().await;
        q = Bit::from(d);
        assert_eq!(dut.q.get(), q, "post-edge value in ReadOnly");
        dut.clk.falling().await;
    }
}
