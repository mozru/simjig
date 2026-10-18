//! Signal access cost (DECISION §4 Q2: 5 ns per access). Ignored by default; run with
//! `cargo nextest run --release -p simjig bench --run-ignored only --no-capture`.

use std::cell::Cell;
use std::hint::black_box;
use std::rc::Rc;
use std::time::Instant;

use simjig::{FakeModel, Precision, RawSig, Sim, Timer};

use super::ticks;

const SIGNALS: usize = 64;
const STEPS: u64 = 2_000;

fn wide_model() -> (Sim, Vec<RawSig>) {
    let mut m = FakeModel::new(Precision::NS);
    for i in 0..SIGNALS {
        m.signal(&format!("s{i}"), 32);
    }
    let sim = Sim::from_fake(m);
    let sigs = (0..SIGNALS)
        .map(|i| sim.signal(&format!("s{i}")).unwrap())
        .collect();
    (sim, sigs)
}

/// Nanoseconds per call of `op` on every signal, `STEPS` timesteps of `SIGNALS` calls.
#[expect(
    clippy::disallowed_methods,
    clippy::cast_precision_loss,
    reason = "a benchmark measures wall-clock time and prints it"
)]
fn per_op(op: fn(RawSig, u64)) -> (f64, f64) {
    let (mut sim, sigs) = wide_model();
    let in_calls = Rc::new(Cell::new(0u128));
    let calls = Rc::clone(&in_calls);
    let start = Instant::now();
    sim.run(async move {
        for step in 0..STEPS {
            let t0 = Instant::now();
            for (i, &sig) in sigs.iter().enumerate() {
                op(sig, step + i as u64);
            }
            calls.set(calls.get() + t0.elapsed().as_nanos());
            Timer::new(ticks(1)).await;
        }
    })
    .unwrap();
    let total = start.elapsed().as_nanos();
    let ops = (STEPS * SIGNALS as u64) as f64;
    (in_calls.get() as f64 / ops, total as f64 / ops)
}

#[test]
#[ignore = "benchmark; run in release with --run-ignored only --no-capture"]
fn signal_access_ns_per_op() {
    // The loop and call overhead of the harness itself, included in every number below.
    let noop = per_op(|s, v| {
        black_box((s, v));
    });
    let write = per_op(|s, v| s.write_u64(black_box(v) & 0xffff_ffff));
    let words = per_op(|s, v| {
        let v = u32::try_from(black_box(v) & 0xffff_ffff).unwrap();
        s.write_words(&[v]);
    });
    let read = per_op(|s, _| {
        black_box(s.read_u64());
    });
    for (what, (call, full)) in [
        ("noop", noop),
        ("write_u64", write),
        ("write_words", words),
        ("read_u64", read),
    ] {
        println!(
            "BENCH {what}: {call:.1} ns per call; {full:.1} ns per op including eval, \
             apply and the timestep"
        );
    }
}
