//! (c) Two models in one test: two different designs share one simulation and one clock,
//! and two revisions of one design run in lockstep until they diverge.

use simjig::{Lockstep, Mirror, ReadOnly, clock, ns};
use simjig_bits::Bits;

use crate::{counter, counter_rev2, derived_clk};

#[simjig::test(timeout = "10us")]
async fn two_designs_share_one_simulation(count: counter::Dut, div: derived_clk::Dut) {
    // One kernel clock per model, coincident edges in one eval.
    clock(count.clk, ns(10)).start();
    clock(div.clk, ns(10)).start();
    count.rst.set(1);
    div.rst.set(1);
    count.en.set(1);
    count.clk.rising().await;
    count.clk.falling().await;
    count.rst.set(0);
    div.rst.set(0);
    for cycle in 1..=40u32 {
        div.clk.rising().await;
        ReadOnly::new().await;
        // Both count the same edges of their own clock; the divided clock half of them.
        assert_eq!(count.count.get(), div.fast_count.get(), "cycle {cycle}");
        assert_eq!(
            div.slow_count.get(),
            Bits::new(u128::from(cycle.div_ceil(2)))
        );
    }
}

#[simjig::test(timeout = "10us")]
async fn a_revision_diverges_from_the_reference_in_lockstep(
    old: counter::Dut,
    new: counter_rev2::Dut,
) {
    let rst = Mirror::new(old.rst, new.rst);
    let en = Mirror::new(old.en, new.en);
    clock(old.clk, ns(10)).start();
    clock(new.clk, ns(10)).start();
    rst.set(1);
    en.set(1);
    old.clk.rising().await;
    old.clk.falling().await;
    rst.set(0);
    let mut lockstep = Lockstep::new().output(old.count, new.count);
    let mut divergence = None;
    for _ in 0..20 {
        old.clk.rising().await;
        ReadOnly::new().await;
        if let Err(d) = lockstep.check() {
            divergence = Some(d);
            break;
        }
    }
    let divergence = divergence.expect("rev2 skips 6");
    assert_eq!(divergence.cycle, 5);
    assert_eq!(divergence.diffs.len(), 1);
    assert_eq!(divergence.diffs[0].path(), "counter.count");
    assert!(
        divergence.to_string().contains(&format!(
            "counter.count: {:?} != {:?}",
            Bits::<8>::new(6),
            Bits::<8>::new(7)
        )),
        "{divergence}"
    );
}
