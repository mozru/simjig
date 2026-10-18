//! The top README's seeded stimulus and scoreboard lines ("Coming from cocotb"), verbatim,
//! type-checked against the real API: a change to `TestCtx::rng`, `Rng`, `Queue` or a
//! signal's `get` that breaks the README breaks this file's build.

use simjig::bits::Bits;
use simjig::{Out, Queue, TestCtx};

/// Stands for any DUT with an output `q` of packed bits.
struct Readme {
    q: Out<Bits<8>>,
}

#[expect(
    dead_code,
    reason = "type-checked, never run: the lines are a sketch of a driver and a monitor"
)]
#[rustfmt::skip] // the README's line breaks
async fn readme_rng_and_queue(ctx: &TestCtx, dut: Readme) {
    let (mut rng, sb) = (ctx.rng("stim"), Queue::new("sb", 0)); // 0: unbounded
    if rng.bool() { sb.put(rng.below(256)).await; }             // the driver records each beat
    assert_eq!(dut.q.get().to_u128(), u128::from(sb.get().await)); // the monitor checks in order
}
