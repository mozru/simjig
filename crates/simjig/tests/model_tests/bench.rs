//! Cost of one signal access through the full typed-handle path: `In<T>::get` / `Out<T>::get`
//! (thread-local kernel lookup, `RefCell`, the backend's dynamic dispatch, the signal-table
//! row, the raw read, `Packed::from_raw`) and `In<T>::set` (the same plus the write log).
//! Budget: 5 ns per access (DECISION §4 Q2). Not a correctness test: run with
//! `cargo test --release -p simjig --test model_tests -- --ignored --nocapture measure`.

use std::hint::black_box;
use std::time::Instant;

use simjig::Timer;
use simjig_bits::Bits;

use super::duts::{counter, wide};
use super::ticks;

const N: u32 = 10_000_000;

#[expect(
    clippy::disallowed_methods,
    reason = "wall-clock measurement, not simulation"
)]
fn per_op(mut f: impl FnMut(u32)) -> f64 {
    let start = Instant::now();
    for i in 0..N {
        f(i);
    }
    start.elapsed().as_secs_f64() * 1e9 / f64::from(N)
}

#[test]
#[ignore = "measurement; run explicitly in release mode"]
fn measure_typed_access() {
    let (mut sim, dut) = counter::Dut::sim().unwrap();
    let (mut wsim, wdut) = wide::Dut::sim().unwrap();
    let narrow = sim.run(async move {
        let mut acc = 0u128;
        let read = per_op(|_| acc ^= black_box(dut.count).get().to_u128());
        let write = per_op(|i| black_box(dut.en).set(i & 1 == 1));
        // Let the write log drain once, so the next run starts clean.
        Timer::new(ticks(1)).await;
        (read, write, black_box(acc))
    });
    let wide = wsim.run(async move {
        let mut acc = 0u128;
        let read8 = per_op(|_| acc ^= black_box(wdut.o8).get().to_u128());
        let mut acc200 = 0u32;
        let read200 = per_op(|_| acc200 ^= black_box(wdut.o200).get().to_words()[3]);
        let write64 = per_op(|i| black_box(wdut.i64).set(Bits::new(u128::from(i))));
        Timer::new(ticks(1)).await;
        (read8, read200, write64, black_box(acc), black_box(acc200))
    });
    let (read, write, _) = narrow.unwrap();
    let (read8, read200, write64, _, _) = wide.unwrap();
    println!(
        "typed get 8-bit {read:.2} ns, 8-bit (wide DUT) {read8:.2} ns, 200-bit {read200:.2} ns; \
         typed set 1-bit {write:.2} ns, 64-bit {write64:.2} ns"
    );
}
