//! `inv`: set an input, let time pass, read the output.

use simjig::{ns, timer};
use simjig_bits::Bit;

use crate::inv;

#[simjig::test]
async fn output_is_the_inverted_input(dut: inv::Dut) {
    for a in [false, true, false, true] {
        dut.a.set(a);
        // A write is a deposit: it is applied after the next eval, so read after a Timer.
        timer(ns(1)).await;
        assert_eq!(dut.a.get(), Bit::from(a));
        assert_eq!(dut.y.get(), Bit::from(!a));
    }
}

#[simjig::test]
async fn a_write_is_not_visible_before_time_passes(dut: inv::Dut) {
    dut.a.set(true);
    assert_eq!(dut.a.get(), Bit::ZERO, "the deposit is not applied yet");
    timer(ns(1)).await;
    assert_eq!((dut.a.get(), dut.y.get()), (Bit::ONE, Bit::ZERO));
}
