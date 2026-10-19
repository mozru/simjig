//! Lockstep: the old `counter` RTL against a second instance of itself (agrees) and against
//! `counter_rev2` (diverges at the first cycle where rev2 skips 6).

use simjig::{Divergence, Lockstep, Mirror, clock, ns, read_only};
use simjig_bits::Bits;

use crate::{counter, counter_rev2};

/// The same stimulus for both: clocks, one cycle of reset, then counting.
async fn reset_then_count(old: counter::Dut, new: counter_rev2::Dut) {
    let rst = Mirror::new(old.rst, new.rst);
    let en = Mirror::new(old.en, new.en);
    clock(old.clk, ns(10)).start();
    clock(new.clk, ns(10)).start();
    rst.set(1);
    en.set(0);
    old.clk.rising().await;
    old.clk.falling().await;
    rst.set(0);
    en.set(1);
}

/// Counts for `cycles` cycles, checking after every rising edge; returns the first
/// divergence.
async fn count_in_lockstep(
    old: counter::Dut,
    new: counter_rev2::Dut,
    cycles: u64,
) -> Result<Lockstep, Divergence> {
    let mut lockstep = Lockstep::new().output(old.count, new.count);
    reset_then_count(old, new).await;
    for _ in 0..cycles {
        old.clk.rising().await;
        read_only().await;
        lockstep.check()?;
    }
    Ok(lockstep)
}

#[simjig::test(timeout = "10us")]
async fn a_different_revision_diverges_at_the_first_differing_cycle(
    old: counter::Dut,
    new: counter_rev2::Dut,
) {
    let divergence = count_in_lockstep(old, new, 20).await.unwrap_err();
    // Cycle k ends with count k + 1 in the old RTL; rev2 goes 5 -> 7 at cycle 5.
    assert_eq!(divergence.cycle, 5);
    assert_eq!(divergence.diffs.len(), 1);
    assert_eq!(
        divergence.diffs[0].path(),
        "counter.count (model 0) vs counter.count (model 1)"
    );
    let text = divergence.to_string();
    assert!(
        text.starts_with("lockstep divergence at cycle 5 (t="),
        "{text}"
    );
    let expected = format!(
        "counter.count (model 0) vs counter.count (model 1): {:?} != {:?}",
        Bits::<8>::new(6),
        Bits::<8>::new(7)
    );
    assert!(text.contains(&expected), "{text}");
}

#[simjig::test(timeout = "10us")]
async fn the_revisions_agree_before_the_change(old: counter::Dut, new: counter_rev2::Dut) {
    let lockstep = count_in_lockstep(old, new, 5).await.unwrap();
    assert_eq!(lockstep.cycle(), 5);
}

#[simjig::test(timeout = "10us")]
async fn two_instances_of_one_build_agree(a: counter::Dut, b: counter::Dut) {
    let rst = Mirror::new(a.rst, b.rst);
    let en = Mirror::new(a.en, b.en);
    let mut lockstep = Lockstep::new().output(a.count, b.count);
    clock(a.clk, ns(10)).start();
    clock(b.clk, ns(10)).start();
    rst.set(1);
    a.clk.rising().await;
    a.clk.falling().await;
    rst.set(0);
    for cycle in 0..300u32 {
        en.set(cycle % 3 != 0);
        a.clk.rising().await;
        read_only().await;
        lockstep.step();
        a.clk.falling().await;
    }
    assert_eq!(lockstep.cycle(), 300);
}

/// `step` fails the test at the divergent cycle, with the diff in the report.
#[simjig::test(timeout = "10us")]
#[should_panic(expected = "lockstep divergence at cycle 5")]
async fn step_fails_the_test_with_the_divergence(old: counter::Dut, new: counter_rev2::Dut) {
    let mut lockstep = Lockstep::new().output(old.count, new.count);
    reset_then_count(old, new).await;
    for _ in 0..20 {
        old.clk.rising().await;
        read_only().await;
        lockstep.step();
    }
}
