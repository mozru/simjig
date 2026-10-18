//! Signal access cost (DECISION §4 Q2: 5 ns per access). Ignored by default; run with
//! `cargo nextest run --release -p simjig bench --run-ignored only --no-capture`.

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;
use std::time::Instant;

use simjig::{FakeModel, Precision, RawSig, Sim, Timer};

use super::ticks;

const SIGNALS: usize = 512;
const STEPS: u64 = 400;

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

/// Nanoseconds per call of `op` over `STEPS` timesteps, each calling it once on every
/// signal: `(median timestep, fastest timestep, whole run including eval and apply)`.
/// Per-timestep figures filter out preemption on a loaded machine.
#[expect(
    clippy::disallowed_methods,
    clippy::cast_precision_loss,
    reason = "a benchmark measures wall-clock time and prints it"
)]
fn per_op(op: fn(RawSig, u64)) -> (f64, f64, f64) {
    let (mut sim, sigs) = wide_model();
    let batches = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&batches);
    let start = Instant::now();
    sim.run(async move {
        for step in 0..STEPS {
            let t0 = Instant::now();
            for (i, &sig) in sigs.iter().enumerate() {
                op(sig, step + i as u64);
            }
            record.borrow_mut().push(t0.elapsed().as_nanos());
            Timer::new(ticks(1)).await;
        }
    })
    .unwrap();
    let total = start.elapsed().as_nanos() as f64;
    let mut batches = batches.take();
    batches.sort_unstable();
    let per = |ns: u128| ns as f64 / SIGNALS as f64;
    let ops = (STEPS * SIGNALS as u64) as f64;
    (
        per(batches[batches.len() / 2]),
        per(batches[0]),
        total / ops,
    )
}

#[test]
#[ignore = "benchmark; run in release with --run-ignored only --no-capture"]
fn signal_access_ns_per_op() {
    // The loop and call overhead of the harness itself, included in every number below.
    let noop = per_op(|s, v| {
        black_box((s, v));
    });
    // Resolving the handle through the current kernel, the floor of every access.
    let width = per_op(|s, _| {
        black_box(s.width());
    });
    let write = per_op(|s, v| s.write_u64(black_box(v) & 0xffff_ffff));
    let words = per_op(|s, v| {
        let v = u32::try_from(black_box(v) & 0xffff_ffff).unwrap();
        s.write_words(&[v]);
    });
    let read = per_op(|s, _| {
        black_box(s.read_u64());
    });
    for (what, result) in [
        ("noop", noop),
        ("width", width),
        ("write_u64", write),
        ("write_words", words),
        ("read_u64", read),
    ] {
        let (median, fastest, full) = result;
        println!(
            "BENCH {what}: {median:.2} ns per call (median; fastest {fastest:.2}); \
             {full:.1} ns per op with eval, apply and the timestep"
        );
    }
}
