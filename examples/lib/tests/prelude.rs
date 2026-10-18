//! `simjig::prelude` next to a sweep module: both glob-imported, every name either brings
//! resolves without an ambiguity error (E0659). `Value`, `Vector` and `Packed` come in
//! through both globs as the same items; this file fails to compile if they ever differ.
#![forbid(unsafe_code)]

use simjig::prelude::*;

simjig::include_dut!(hop_stage);

use hop_stage::*;

/// Names `Value`, `Vector` and `Packed`, which both globs bring, and the sweep's
/// `BeatFields`.
fn width_and_tag<B: BeatFields + Value>(beat: B) -> (u32, u128) {
    (<B as Packed>::WIDTH, beat.tag().to_u128())
}

fn low_bit<V: Vector>(v: V) -> bool {
    v.to_u128() & 1 == 1
}

#[simjig::test(timeout = "1us", duts = [hop_8x2::Dut, hop_32x4::Dut])]
async fn the_prelude_and_a_sweep_glob_import_together<D: Ports>(dut: D) {
    clock(dut.clk(), ns(10)).start();
    dut.rst().set(1);
    dut.up().valid().set(0);
    cycles(dut.clk(), 1).read_write().await;
    let beat = D::Beat::new(1, 2);
    let (width, tag) = width_and_tag(beat);
    assert_eq!(width, D::WIDTH.unsigned_abs() + D::TAG_W.unsigned_abs());
    assert_eq!(tag, 1);
    assert!(low_bit(Bits::<2>::new(3)));
    assert!(low_bit(Bit::new(true)));
}
