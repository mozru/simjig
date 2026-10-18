//! A combinational design: drive an input, let time pass, check the output.

use quickstart::*;

#[simjig::test]
async fn y_is_not_a(dut: inv::Dut) {
    for a in [false, true] {
        dut.a.set(a);
        // Writes are applied at the next evaluation; wait for it before reading.
        Timer::new(ns(1)).await;
        assert_eq!(dut.y.get(), Bit::from(!a));
    }
}
