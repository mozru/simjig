//! A combinational design: drive an input, let time pass, check the output.

use simjig::{Timer, ns};
use simjig_bits::Bit;

// The bindings `build.rs` generated: `inv::Dut`, one typed handle per port.
simjig::include_dut!(inv);

#[simjig::test]
async fn y_is_not_a(dut: inv::Dut) {
    for a in [false, true] {
        dut.a.set(a);
        // Writes are applied at the next evaluation; wait for it before reading.
        Timer::new(ns(1)).await;
        assert_eq!(dut.y.get(), Bit::from(!a));
    }
}
