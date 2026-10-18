//! `iface_top`: a DUT whose data ports are interface modports, built through the generated
//! wrapper (`bind_iface` in build.rs). Each port is a bundle typed from its modport: the
//! testbench drives the `dst` side's inputs and reads its `ready`; on the `src` side it is
//! the reverse.

use simjig::{In, Out, ReadOnly, clock, ns};
use simjig_bits::{Bit, Bits};

use crate::iface_top::{Dut, Kind};

#[simjig::test(timeout = "1us")]
async fn a_beat_crosses_the_slice_incremented(dut: Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.in_bus.valid.set(0);
    dut.out_bus.ready.set(1);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);

    dut.in_bus.valid.set(1);
    dut.in_bus.data.set(0x41);
    dut.in_bus.kind.set(Kind::DATA);
    dut.clk.rising().await;
    ReadOnly::new().await;
    assert_eq!(dut.in_bus.ready.get(), Bit::ONE);
    assert_eq!(dut.out_bus.valid.get(), Bit::ONE);
    assert_eq!(dut.out_bus.data.get(), Bits::new(0x42));
    assert_eq!(dut.out_bus.kind.get(), Kind::DATA);
}

/// The bundle fields are typed from the modports, from the testbench's side: what the DUT
/// reads is an `In` (writable), what it drives is an `Out`. These bindings only compile with
/// exactly these handle types, and an `Out` has no `set` (the `compile_fail` doctest on
/// `simjig::Out`), so writing an `Out` field does not compile.
#[simjig::test]
async fn bundle_fields_take_their_direction_from_the_modport(dut: Dut) {
    let _: In<Bit> = dut.in_bus.valid;
    let _: In<Bits<16>> = dut.in_bus.data;
    let _: In<Kind> = dut.in_bus.kind;
    let _: Out<Bit> = dut.in_bus.ready;
    let _: Out<Bit> = dut.out_bus.valid;
    let _: Out<Bits<16>> = dut.out_bus.data;
    let _: Out<Kind> = dut.out_bus.kind;
    let _: In<Bit> = dut.out_bus.ready;
    assert_eq!(
        dut.out_bus.valid.name(),
        "simjig_wrap_iface_top.out_bus_valid"
    );
}
