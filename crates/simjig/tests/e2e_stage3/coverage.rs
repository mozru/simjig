//! (h) Coverage counters read from Rust: `counter_cov` is `tests/duts/counter` built with
//! `--coverage`; `Sim::coverage` copies its counters (the ABI's `cov_runs`) between runs.
//! They start at zero, grow with the stimulus, and a model without coverage has none.

use simjig::{DutModel, ModelOptions, ReadOnly, Seed, SeedSource, Sim, clock, ns};

use crate::{counter, counter_cov};

fn options() -> ModelOptions {
    ModelOptions::new(Seed::new(1, SeedSource::Env))
}

/// Resets, then counts with `en` for `cycles` rising edges.
fn count(sim: &mut Sim, dut: counter_cov::Dut, cycles: u32) {
    sim.run(async move {
        clock(dut.clk, ns(10)).start();
        dut.rst.set(1);
        dut.en.set(1);
        dut.clk.falling().await;
        dut.rst.set(0);
        for _ in 0..cycles {
            dut.clk.rising().await;
        }
        ReadOnly::new().await;
    })
    .unwrap();
}

#[test]
fn counters_start_at_zero_and_count_the_stimulus() {
    let mut sim = counter_cov::Dut::instantiate(&options()).unwrap();
    let dut = counter_cov::Dut::create(&mut sim).unwrap();
    let before = sim.coverage(0).expect("a model built with coverage");
    assert!(
        before.len() > 1 && before.iter().all(|&c| c == 0),
        "{before:?}"
    );
    count(&mut sim, dut, 25);
    let after = sim.coverage(0).unwrap();
    assert_eq!(
        after.len(),
        before.len(),
        "the layout is fixed for one build"
    );
    // The `else if (en)` branch ran on every rising edge after reset: 25 times. (Line
    // and branch points are one counter each; toggle points count bit flips.)
    assert!(after.contains(&25), "{after:?}");
    assert!(after.iter().sum::<u32>() > 25);
}

#[test]
fn a_model_without_coverage_has_no_counters() {
    let mut sim = counter::Dut::instantiate(&options()).unwrap();
    let _dut = counter::Dut::create(&mut sim).unwrap();
    assert_eq!(sim.coverage(0), None);
    assert_eq!(sim.coverage(1), None, "no such model");
}
