//! (g) `iface_top`, whose data ports are interface modports, through the generated
//! wrapper, with two read-only probes of the slice's internal state (`full`, `data_q`)
//! named from the DUT's own top (`iface_top.full`) and bound behind the wrapper.

use simjig::bits::{Bit, Bits};
use simjig::{clock, ns, read_only};

use crate::iface_top::{Dut, Kind};

#[simjig::test(timeout = "1us")]
async fn probes_see_the_slice_fill_and_drain(dut: Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.in_bus.valid.set(0);
    dut.out_bus.ready.set(0);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    read_only().await;
    assert_eq!(dut.full.get(), Bit::ZERO);

    dut.clk.falling().await;
    dut.in_bus.valid.set(1);
    dut.in_bus.data.set(0x1233);
    dut.in_bus.kind.set(Kind::DATA);
    dut.clk.rising().await;
    read_only().await;
    // Accepted: the probes see the register the output reads.
    assert_eq!(dut.full.get(), Bit::ONE);
    assert_eq!(dut.data_q.get(), Bits::new(0x1234));
    assert_eq!(dut.out_bus.data.get(), dut.data_q.get());
    // Full and the sink is not ready: back-pressure.
    assert_eq!(dut.in_bus.ready.get(), Bit::ZERO);

    dut.clk.falling().await;
    dut.in_bus.valid.set(0);
    dut.out_bus.ready.set(1);
    dut.clk.rising().await;
    read_only().await;
    assert_eq!(dut.full.get(), Bit::ZERO, "the beat left");
    // Named from the DUT's top, as declared in build.rs, not from the wrapper.
    assert_eq!(dut.full.name(), "iface_top.full");
}
