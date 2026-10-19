//! (b) `timing_dut` (`--timing`): the design's own `#5` clock and `#23` pulse, a kernel
//! clock on `start` and a testbench `Timer` loop, all in one run. The kernel's timesteps
//! are strictly increasing, and they are exactly the union of every source's event times:
//! none is skipped, none is invented.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use simjig::{
    DutModel, ModelOptions, Seed, SeedSource, SimTime, TraceKind, clock, now, ns, read_only, spawn,
    timer,
};
use simjig_bits::Bit;

use crate::timing_dut::Dut;

/// ps per ns (the design's precision is 1 ps).
const NS: u64 = 1_000;
const END: u64 = 300 * NS;
/// The kernel clock on `start` begins here (a rising edge), period 60 ns.
const CLOCK_START: u64 = 2 * NS;
/// The testbench's timer loop wakes every 7 ns.
const TIMER: u64 = 7 * NS;

/// Rising `iclk` edges up to and including `t`: 5 ns, 15 ns, ...
fn iclk_rises(t: u64) -> u64 {
    if t < 5 * NS {
        0
    } else {
        (t - 5 * NS) / (10 * NS) + 1
    }
}

#[test]
fn timers_a_kernel_clock_and_design_delays_interleave_without_skipping_a_timestep() {
    let options = ModelOptions::new(Seed::new(3, SeedSource::Env));
    let mut sim = Dut::instantiate(&options).unwrap();
    let dut = Dut::create(&mut sim).unwrap();
    let samples = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&samples);
    sim.run(async move {
        dut.start.set(0);
        timer(ns(2)).await;
        clock(dut.start, ns(60)).start();
        spawn("sampler", async move {
            loop {
                timer(ns(7)).await;
                read_only().await;
                seen.borrow_mut()
                    .push((now().ticks(), dut.ticks.get().to_u128(), dut.done.get()));
            }
        })
        .detach();
        timer(ns(298)).await;
    })
    .unwrap();

    let steps: Vec<u64> = sim
        .recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect();
    assert!(
        steps.windows(2).all(|w| w[0] < w[1]),
        "monotonic: {steps:?}"
    );

    let mut expected = BTreeSet::from([0, CLOCK_START, END]);
    expected.extend((1..).map(|k| k * 5 * NS).take_while(|&t| t <= END)); // iclk
    expected.extend(
        (1..)
            .map(|k| CLOCK_START + k * 30 * NS)
            .take_while(|&t| t <= END),
    );
    expected.extend(
        (1..)
            .map(|k| CLOCK_START + k * TIMER)
            .take_while(|&t| t <= END),
    );
    // `done` rises 23 ns after each rising edge of `start`.
    expected.extend(
        (0..)
            .map(|k| CLOCK_START + k * 60 * NS + 23 * NS)
            .take_while(|&t| t <= END),
    );
    assert_eq!(steps, expected.into_iter().collect::<Vec<_>>());

    // What the sampler saw at each 7 ns wake: the design's own clock count, and the pulse.
    for &(t, ticks, done) in samples.borrow().iter() {
        assert_eq!(
            u64::try_from(ticks).unwrap(),
            iclk_rises(t) % 256,
            "ticks at {t}"
        );
        let since_rise = (t - CLOCK_START) % (60 * NS);
        let low = since_rise < 23 * NS;
        assert_eq!(done, Bit::new(!low), "done at {t}");
    }
    assert!(samples.borrow().len() > 40);
    assert_eq!(sim.now(), SimTime(END));
}
