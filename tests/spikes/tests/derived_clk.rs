//! Experiment 5: can a testbench that samples a derived clock between evals miss a pulse?
//!
//! - `Vderived`: tests/duts/derived_clk with rtl/derived_clk.vlt (`div_clk`
//!   public_flat_rd). div_clk is a flop output, so it changes at most once per eval.
//! - `Vglitch`: rtl/glitch.sv, an internal derived clock that rises and falls inside one
//!   eval (self-clearing flop), also public_flat_rd.
//!
//! "Observed" means: the value read after eval() returns, compared with the value read
//! after the previous sampled eval, which is all a kernel can do (DECISION.md §1 step 3).

use simjig_spikes::{Context, Model, VTable};

unsafe extern "C" {
    fn spk_vtable_Vderived() -> *const VTable;
    fn spk_vtable_Vglitch() -> *const VTable;
}

// derived_clk ports (shim/derived.cpp).
const CLK: u32 = 0;
const RST: u32 = 1;
const DIV_CLK: u32 = 2;
const SLOW: u32 = 4;

/// Runs `cycles` clk cycles (two evals each) and samples div_clk after every `stride`-th
/// eval. Returns (rising edges the design counted, rising edges observed, mismatches
/// between the port and its public_flat_rd copy).
fn derived_run(cycles: u64, stride: u64) -> (u64, u64, u64) {
    let ctx = Context::new(1);
    // SAFETY: table from the linked Vderived shim.
    let mut m = unsafe { Model::new(spk_vtable_Vderived(), &ctx) };
    m.eval();
    m.poke(RST, 1);
    for level in [1, 0] {
        m.poke(CLK, level);
        m.eval();
    }
    m.poke(RST, 0);
    m.eval();

    let (mut design, mut observed, mut mismatches) = (0, 0, 0);
    let mut last_slow = m.peek(SLOW);
    let mut last_seen = m.peek(DIV_CLK);
    for eval in 1..=cycles * 2 {
        m.poke(CLK, eval % 2);
        m.eval();
        // The design's own count of div_clk rising edges (8-bit, so per eval).
        let slow = m.peek(SLOW);
        design += (slow + 256 - last_slow) % 256;
        last_slow = slow;
        let port = m.peek(DIV_CLK);
        mismatches += u64::from(port != m.internal_op(0, 0));
        if eval % stride == 0 {
            observed += u64::from(last_seen == 0 && port == 1);
            last_seen = port;
        }
    }
    (design, observed, mismatches)
}

#[test]
fn derived_clock_sampled_after_every_eval_is_never_missed() {
    let (design, observed, mismatches) = derived_run(10_000, 1);
    eprintln!(
        "SPIKE derived_clk stride 1: design {design} edges, observed {observed}, port/public mismatches {mismatches}"
    );
    assert_eq!(design, 5_000);
    assert_eq!(observed, design);
    assert_eq!(mismatches, 0);
}

#[test]
fn derived_clock_sampled_less_often_than_every_eval_aliases() {
    let mut table = Vec::new();
    for stride in 1..=4 {
        let (design, observed, _) = derived_run(10_000, stride);
        table.push((stride, observed, design));
    }
    eprintln!("SPIKE derived_clk (stride evals, observed, design): {table:?}");
    // One clk cycle = 2 evals; div_clk period = 4 evals.
    assert_eq!(
        table[1].1, table[1].2,
        "every 2nd eval still sees every div_clk edge"
    );
    assert_eq!(
        table[3].1, 0,
        "every 4th eval samples the same div_clk phase"
    );
}

#[test]
fn pulse_inside_one_eval_is_invisible_to_sampling() {
    let ctx = Context::new(1);
    // SAFETY: table from the linked Vglitch shim.
    let mut m = unsafe { Model::new(spk_vtable_Vglitch(), &ctx) };
    m.eval();
    let cycles = 1_000u64;
    let mut seen_high = 0;
    for eval in 1..=cycles * 2 {
        m.poke(0, eval % 2);
        m.eval();
        seen_high += m.internal_op(0, 0); // glitch__DOT__pulse after eval
    }
    let design = m.peek(1);
    eprintln!(
        "SPIKE glitch: design counted {design} pulses (mod 256), testbench saw pulse=1 {seen_high} times in {} evals",
        cycles * 2
    );
    assert_eq!(design, cycles % 256);
    assert_eq!(seen_high, 0);
}
