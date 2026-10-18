//! `hop_stage` (`rtl/hop_stage.sv`): a pipeline stage over `tagged_if` beats, built as a
//! sweep of two configurations whose `beat_t` has different field widths. One test body,
//! generic over `hop_stage::Ports`, runs on both: `D::Beat` is the type of `up.beat` and of
//! `down.beat`, built with `D::Beat::new(tag, data)` and read with `beat.tag()`
//! (`hop_stage::BeatFields`); a field and a port of one type in every configuration
//! (`beat.data()`, `down_data`) are one type, `D::DownData`. The glob import is the body's only one for the sweep: it
//! brings the sweep's traits and the runtime's `Value`, `Vector` and `Packed`.

use simjig::{ReadOnly, clock, ns};

mod common;

simjig::include_dut!(hop_stage); // the sweep: hop_stage::{Ports, BeatFields, hop_8x2, hop_32x4}

use hop_stage::*;

/// A beat crosses the stage in one cycle with its hop count incremented, wrapping at the
/// tag's width, in both configurations (the top README's "A parameter sweep").
#[simjig::test(init = random, timeout = "1us", duts = [hop_8x2::Dut, hop_32x4::Dut])]
async fn a_beat_hops_with_its_tag_incremented<D: Ports>(dut: D) {
    clock(dut.clk(), ns(10)).start();
    dut.rst().set(1);
    dut.up().valid().set(0);
    dut.clk().rising().await;
    dut.clk().falling().await;
    dut.rst().set(0);
    let wrap = 1u128 << D::TAG_W;
    for (tag, data) in [(0, 0x5a), (wrap - 1, 0x3c)] {
        let beat = D::Beat::new(tag, data); // fields in declaration order
        dut.up().beat().set(beat);
        dut.up().valid().set(1);
        dut.clk().rising().await;
        ReadOnly::new().await;
        let hopped: D::Beat = dut.down().beat().get(); // one type on both ports
        assert_eq!(hopped, beat.with_tag((beat.tag().to_u128() + 1) % wrap));
        assert_eq!(hopped.data(), beat.data());
        let data: D::DownData = dut.down_data().get(); // the field's type is the port's
        assert_eq!(data, hopped.data());
        assert_eq!(dut.down().valid().get(), 1);
        dut.clk().falling().await;
    }
}

/// As a top, the module's interface ports get `tagged_if` instances with the interface's
/// defaults (`WIDTH` 8, `TAG_W` 2): other module parameters disagree with their beats.
#[test]
fn parameters_are_checked_at_elaboration() {
    let src = &["rtl/tagged_if.sv", "rtl/hop_stage.sv"];
    let good: &[&[&str]] = &[&[], &["WIDTH=8", "TAG_W=2"]];
    let bad: &[&[&str]] = &[&["TAG_W=3"], &["WIDTH=16"]];
    common::assert_param_checks(
        "hop_stage",
        src,
        good,
        bad,
        "disagree with the interfaces' beats",
    );
}
