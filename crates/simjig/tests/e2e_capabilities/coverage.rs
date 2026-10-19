//! (h) Coverage counters read from Rust: `counter_cov` is `tests/duts/counter` built with
//! `--coverage`; `Sim::coverage` copies its counters (the ABI's `cov_runs`) between runs.
//! They start at zero, grow with the stimulus, and a model without coverage has none.
//! `zero_coverage` clears them and `write_coverage` writes Verilator's `coverage.dat`,
//! which names every point; a `#[simjig::test]` reaches all three through its `TestCtx`.

use std::path::PathBuf;

use simjig::{
    CoverageError, DutModel, ModelOptions, Seed, SeedSource, Sim, TestCtx, clock, ns, read_only,
};

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
        read_only().await;
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

/// The points of a `coverage.dat`: each `C '<keys>' <count>` line's keys (`\x01key\x02value`
/// pairs) and count.
fn points(dat: &str) -> Vec<(Vec<(String, String)>, u32)> {
    dat.lines()
        .filter_map(|line| line.strip_prefix("C '"))
        .map(|rest| {
            let (keys, count) = rest.rsplit_once("' ").expect("a count after the keys");
            let keys = keys
                .split('\x01')
                .filter(|kv| !kv.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('\x02').expect("key\\x02value");
                    (k.to_owned(), v.to_owned())
                })
                .collect();
            (keys, count.trim().parse().expect("a count"))
        })
        .collect()
}

/// Counts 10 cycles, zeroes the counters, counts 25 more and writes `coverage.dat`.
#[simjig::test(timeout = "10us")]
async fn counters_can_be_zeroed_and_written_with_their_names(ctx: &TestCtx, dut: counter_cov::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(1);
    dut.clk.falling().await;
    dut.rst.set(0);
    for _ in 0..10 {
        dut.clk.rising().await;
    }
    read_only().await;
    assert!(ctx.coverage(0).unwrap().contains(&10));
    ctx.zero_coverage(0).unwrap();
    assert!(ctx.coverage(0).unwrap().iter().all(|&c| c == 0));
    for _ in 0..25 {
        dut.clk.rising().await;
    }
    read_only().await;
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e-capabilities-coverage.dat");
    std::fs::remove_file(&path).ok();
    ctx.write_coverage(0, &path).unwrap();
    let counters = ctx.coverage(0).unwrap();

    let dat = std::fs::read_to_string(&path).expect("coverage.dat was written");
    let points = points(&dat);
    assert!(!points.is_empty(), "{dat}");
    for (keys, _) in &points {
        let key = |k: &str| {
            keys.iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.as_str())
        };
        assert!(
            key("f").is_some_and(|f| f.ends_with("counter.sv")),
            "{keys:?}"
        );
        assert!(key("l").is_some() && key("h").is_some(), "{keys:?}");
        assert!(key("page").is_some_and(|p| p.starts_with("v_")), "{keys:?}");
    }
    // Every counter that counted is one named point, with the count Rust reads. (Verilator
    // writes one line per point name, so a few zero counters the runs include, such as
    // points that share a name, have no line of their own.)
    let mut written: Vec<u32> = points.iter().map(|(_, count)| *count).collect();
    let mut read: Vec<u32> = counters.iter().copied().filter(|&c| c > 0).collect();
    written.retain(|&c| c > 0);
    written.sort_unstable();
    read.sort_unstable();
    assert_eq!(written, read, "{dat}");
    assert!(points.len() <= counters.len());
    assert!(
        read.contains(&25),
        "the zeroed counters count only the last 25 cycles"
    );
}

#[test]
fn coverage_errors_name_the_model_and_the_fix() {
    let mut sim = counter::Dut::instantiate(&options()).unwrap();
    let _dut = counter::Dut::create(&mut sim).unwrap();
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e-capabilities-none.dat");
    let error = sim.write_coverage(0, &path).unwrap_err();
    assert_eq!(error, CoverageError::NotBuilt { model: 0 });
    assert!(error.to_string().contains(".coverage(true)"), "{error}");
    assert_eq!(
        sim.zero_coverage(1),
        Err(CoverageError::NoModel { model: 1, count: 1 })
    );

    let mut sim = counter_cov::Dut::instantiate(&options()).unwrap();
    let dut = counter_cov::Dut::create(&mut sim).unwrap();
    let error = sim
        .write_coverage(0, "/nonexistent-dir/coverage.dat")
        .unwrap_err();
    assert!(
        matches!(error, CoverageError::Write { model: 0, .. }),
        "{error}"
    );
    sim.zero_coverage(0).unwrap();
    count(&mut sim, dut, 5);
    assert!(sim.coverage(0).unwrap().contains(&5));
    sim.write_coverage(0, &path).unwrap();
    assert!(path.is_file());
}

/// Each fork branch counts on from the checkpoint's counters in its own copy of the model
/// (one zeroes them), and the parent's counters are untouched by every branch.
#[simjig::test(timeout = "10us")]
async fn each_fork_branch_counts_its_own_coverage(ctx: &TestCtx, dut: counter_cov::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(1);
    dut.clk.falling().await;
    dut.rst.set(0);
    for _ in 0..10 {
        dut.clk.rising().await;
    }
    read_only().await;
    let at_checkpoint = ctx.coverage(0).unwrap();
    // The `else if (en)` branch's counter: one per rising edge after reset.
    let en = at_checkpoint
        .iter()
        .position(|&c| c == 10)
        .expect("the en counter");
    let branches = simjig::fork::run_from_checkpoint(ctx.seed(), 4, |b| async move {
        if b.index() == 3 {
            ctx.zero_coverage(0).unwrap();
        }
        for _ in 0..5 * (b.index() + 1) {
            dut.clk.rising().await;
        }
        read_only().await;
        Ok(ctx.coverage(0).unwrap())
    })
    .await
    .unwrap();
    assert_eq!(
        ctx.coverage(0).unwrap(),
        at_checkpoint,
        "the parent's counters"
    );
    let counts: Vec<u32> = branches.into_values().iter().map(|c| c[en]).collect();
    assert_eq!(counts, [15, 20, 25, 20]);
    for _ in 0..3 {
        dut.clk.rising().await;
    }
    read_only().await;
    assert_eq!(ctx.coverage(0).unwrap()[en], 13);
}
