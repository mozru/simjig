//! Experiment 4: a `--timing` model driven by a hand-written loop that merges the design's
//! own schedule (`eventsPending()`/`nextTimeSlot()`) with a testbench-driven clock.
//!
//! The loop is the normative per-timestamp loop of DECISION.md §1 reduced to its time
//! handling: next = min(testbench wheel, nextTimeSlot() if eventsPending()); set the
//! context time; apply testbench edges due at `next`; one eval.
//!
//! Time unit: the context's time precision, 1 ps for timing_dut (`timeprecision 1ps`).

use simjig_spikes::fork::{ChildEnd, in_child};
use simjig_spikes::{Context, Model, VTable};

unsafe extern "C" {
    fn spk_vtable_Vtiming() -> *const VTable;
    fn spk_vtable_Vcounter() -> *const VTable;
}

// timing_dut ports (shim/timing.cpp).
const START: u32 = 0;
const ICLK: u32 = 1;
const TICKS: u32 = 2;
const DONE: u32 = 3;

const NS: u64 = 1000;

fn timing(ctx: &Context) -> Model<'_> {
    // SAFETY: table from the linked Vtiming shim.
    unsafe { Model::new(spk_vtable_Vtiming(), ctx) }
}

/// One evaluated time step: (time, start, iclk, ticks, done, design slot?, tb edge?).
#[derive(Debug, Clone, Copy)]
struct Step {
    t: u64,
    iclk: u64,
    ticks: u64,
    done: u64,
    design: bool,
    tb: bool,
}

/// Runs the merged loop until `end`. The testbench toggles `start` every `half` ps.
fn merged_loop(ctx: &Context, m: &mut Model<'_>, half: u64, end: u64) -> Vec<Step> {
    ctx.set_time(0);
    m.eval(); // time 0: initial blocks run and schedule their first delays
    let mut steps = Vec::new();
    let mut tb_next = half;
    let mut start = 0;
    loop {
        // Only ask for the next slot when one exists (experiment: it exits otherwise).
        let design_next = m.events_pending().then(|| m.next_time_slot_unchecked());
        let next = design_next.map_or(tb_next, |d| d.min(tb_next));
        if next > end {
            break;
        }
        assert!(
            next >= ctx.time(),
            "time went backwards: {} -> {next}",
            ctx.time()
        );
        ctx.set_time(next);
        let tb = next == tb_next;
        if tb {
            start ^= 1;
            m.poke(START, start);
            tb_next += half;
        }
        m.eval();
        steps.push(Step {
            t: next,
            iclk: m.peek(ICLK),
            ticks: m.peek(TICKS),
            done: m.peek(DONE),
            design: design_next == Some(next),
            tb,
        });
    }
    steps
}

#[test]
fn merged_loop_keeps_time_monotonic_and_matches_the_readme() {
    let ctx = Context::new(1);
    let mut m = timing(&ctx);
    // A 7 ns testbench half period interleaves with the design's 5 ns slots and
    // coincides with them at multiples of 35 ns.
    let steps = merged_loop(&ctx, &mut m, 7 * NS, 100 * NS);
    for w in steps.windows(2) {
        assert!(
            w[1].t > w[0].t,
            "two evals at one time, or backwards: {w:?}"
        );
    }
    let at = |t: u64| {
        steps
            .iter()
            .rev()
            .find(|s| s.t <= t)
            .copied()
            .expect("step")
    };
    // README: 5 iclk rising edges by 50 ns (5, 15, 25, 35, 45).
    assert_eq!(at(50 * NS).ticks, 5);
    // start rises at 7 ns: done falls then, and rises 23 ns later, at 30 ns.
    assert_eq!(
        (at(7 * NS).done, at(30 * NS - 1).done, at(30 * NS).done),
        (0, 0, 1)
    );
    // iclk toggles only at the design's own 5 ns slots.
    for w in steps.windows(2).filter(|w| w[0].iclk != w[1].iclk) {
        assert!(w[1].design && w[1].t % (5 * NS) == 0, "{w:?}");
    }
    let shared = steps.iter().filter(|s| s.design && s.tb).count();
    eprintln!(
        "SPIKE timing loop: {} evals to 100 ns, {} design slots, {} tb edges, {shared} shared",
        steps.len(),
        steps.iter().filter(|s| s.design).count(),
        steps.iter().filter(|s| s.tb).count()
    );
    assert!(
        shared >= 2,
        "coincident design slot and tb edge share one eval"
    );
}

/// Runs `body` in a child and returns how it ended (the model call may exit(1)).
fn ends(body: impl FnOnce()) -> ChildEnd {
    in_child(|| {
        body();
        String::new()
    })
    .end
}

#[test]
fn next_time_slot_exits_the_process_when_no_event_is_pending() {
    // A model without --timing: eventsPending() is always false.
    let ctx = Context::new(1);
    // SAFETY: table from the linked Vcounter shim.
    let mut c = unsafe { Model::new(spk_vtable_Vcounter(), &ctx) };
    c.eval();
    assert!(!c.events_pending());
    let no_timing = ends(|| {
        c.next_time_slot_unchecked();
    });

    // A --timing model before its first eval: nothing is scheduled yet.
    let tctx = Context::new(1);
    let mut t = timing(&tctx);
    assert!(!t.events_pending());
    let before_first_eval = ends(|| {
        t.next_time_slot_unchecked();
    });
    t.eval();
    assert!(t.events_pending());
    eprintln!(
        "SPIKE nextTimeSlot without events: no --timing -> {no_timing:?}; before first eval -> {before_first_eval:?}"
    );
    assert_eq!(no_timing, ChildEnd::Exited(1));
    assert_eq!(before_first_eval, ChildEnd::Exited(1));
}

#[test]
fn skipping_past_a_design_slot_is_fatal_but_going_backwards_is_not_checked() {
    let ctx = Context::new(1);
    let mut m = timing(&ctx);
    ctx.set_time(0);
    m.eval();
    assert!(m.events_pending());
    let slot = m.next_time_slot_unchecked();
    assert_eq!(slot, 5 * NS);
    // Jump over the 5 ns slot: Verilator's delay scheduler finds a process due earlier.
    let skipped = ends(|| {
        ctx.set_time(slot + 1);
        m.eval();
    });
    // Go back in time after evaluating at 5 ns: Verilator does not notice.
    let backwards = in_child(|| {
        ctx.set_time(slot);
        m.eval();
        ctx.set_time(2 * NS);
        m.eval();
        format!("{} {}", m.events_pending(), m.next_time_slot_unchecked())
    });
    eprintln!(
        "SPIKE skip slot -> {skipped:?}; backwards -> {:?} report {:?}",
        backwards.end, backwards.report
    );
    assert_eq!(skipped, ChildEnd::Exited(1));
    assert_eq!(
        backwards.end,
        ChildEnd::Exited(0),
        "no check for backwards time"
    );
}
