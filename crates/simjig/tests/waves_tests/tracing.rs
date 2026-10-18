//! `TraceOptions` on real FST models: the file, its timestamps, start and end windows,
//! depth, waves up to a failure, and the error for a model built without FST.

use std::path::PathBuf;

use simjig::{
    DutModel, Failure, ModelOptions, ReadOnly, Seed, SeedSource, Sim, SimTime, TraceKind,
    TraceOptions, clock, ns,
};
use simjig_bits::Bit;

use crate::fst::{self, Fst};
use crate::{counter, counter_fst, fifo_fst};

fn options() -> ModelOptions {
    ModelOptions::new(Seed::new(1, SeedSource::Env))
}

fn waves_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("waves");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join(format!("{name}.fst"));
    std::fs::remove_file(&path).ok();
    path
}

/// The kernel's timesteps of `sim`, from its recorder.
fn timesteps(sim: &Sim) -> Vec<u64> {
    sim.recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect()
}

/// Runs the counter for `cycles` cycles of a 10 ns clock (enabled after one reset cycle),
/// optionally panicking at the last one; returns the timesteps, the run's result, and the
/// waves once the model is dropped.
fn run_counter(
    trace: TraceOptions,
    cycles: u32,
    fail: bool,
) -> (Vec<u64>, Result<(), Failure>, Fst) {
    let path = trace.path().to_path_buf();
    let mut sim = counter_fst::Dut::instantiate(&options().with_trace(trace)).expect("loads");
    let dut = counter_fst::Dut::create(&mut sim).expect("binds");
    let result = sim.run(async move {
        clock(dut.clk, ns(10)).start();
        dut.rst.set(1);
        dut.en.set(1);
        dut.clk.falling().await;
        dut.rst.set(0);
        for _ in 0..cycles {
            dut.clk.rising().await;
        }
        ReadOnly::new().await;
        assert!(!fail, "count reached {}", dut.count.get().to_u128());
    });
    let steps = timesteps(&sim);
    drop(sim);
    (steps, result, fst::read(&path))
}

#[test]
fn a_traced_run_writes_an_fst_whose_header_parses() {
    let path = waves_path("header");
    let (steps, result, fst) = run_counter(TraceOptions::new(&path), 20, false);
    result.expect("passes");
    // tests/duts are built with `--timescale 1ns/1ps`: one tick is 1 ps.
    assert_eq!(fst.timescale, -12);
    assert!(fst.vars >= 4, "clk, rst, en, count at least: {fst:?}");
    assert!(fst.blocks >= 1, "{fst:?}");
    assert_eq!(fst.start, 0);
    assert_eq!(Some(&fst.end), steps.last());
}

#[test]
fn every_timestep_is_one_sample_at_its_sim_time() {
    let path = waves_path("timestamps");
    let (steps, result, fst) = run_counter(TraceOptions::new(&path), 20, false);
    result.expect("passes");
    assert!(steps.len() > 40, "{steps:?}");
    assert_eq!(fst.times, steps);
    // Edges every 5 ns of a 10 ns clock, in ps.
    assert_eq!(&fst.times[..4], [0, 5_000, 10_000, 15_000]);
}

#[test]
fn a_start_time_skips_the_samples_before_it() {
    let path = waves_path("start");
    let start = 52_000;
    let (steps, result, fst) =
        run_counter(TraceOptions::new(&path).start(SimTime(start)), 20, false);
    result.expect("passes");
    let kept: Vec<u64> = steps.iter().copied().filter(|&t| t >= start).collect();
    assert_eq!(fst.times, kept);
    assert_eq!(fst.start, 55_000, "the first timestep at or after 52 ns");
}

#[test]
fn an_end_time_stops_sampling_after_it() {
    let path = waves_path("end");
    let end = 100_000;
    let (steps, result, fst) = run_counter(
        TraceOptions::new(&path)
            .start(SimTime(30_000))
            .end(SimTime(end)),
        20,
        false,
    );
    result.expect("passes");
    let kept: Vec<u64> = steps
        .iter()
        .copied()
        .filter(|&t| (30_000..=end).contains(&t))
        .collect();
    assert_eq!(fst.times, kept);
    assert_eq!(fst.end, end);
}

#[test]
fn a_depth_limit_records_fewer_signals() {
    let all = run_counter(TraceOptions::new(waves_path("depth0")), 2, false).2;
    let top = run_counter(TraceOptions::new(waves_path("depth1")).depth(1), 2, false).2;
    assert!(top.vars < all.vars, "depth 1: {top:?}, all: {all:?}");
    assert!(top.vars > 0, "{top:?}");
}

#[test]
fn the_waves_of_a_failed_run_end_at_the_failure() {
    let path = waves_path("failure");
    let (steps, result, fst) = run_counter(TraceOptions::new(&path), 7, true);
    let failure = result.expect_err("the body panics");
    assert_eq!(fst.times, steps);
    assert_eq!(fst.end, failure.time.ticks());
}

#[test]
fn a_model_built_without_fst_says_how_to_enable_it() {
    let trace = TraceOptions::new(waves_path("plain"));
    let error = counter::Dut::instantiate(&options().with_trace(trace)).expect_err("no FST");
    assert!(
        error.message.contains("was built without FST tracing")
            && error.message.contains(".trace_fst(true)"),
        "{}",
        error.message
    );
    // Optional waves fall back to an untraced run.
    let optional = TraceOptions::new(waves_path("plain")).if_supported();
    assert!(counter::Dut::instantiate(&options().with_trace(optional)).is_ok());
}

#[test]
fn a_fifo_stream_is_traced_too() {
    let path = waves_path("fifo");
    let mut sim =
        fifo_fst::Dut::instantiate(&options().with_trace(TraceOptions::new(&path))).expect("loads");
    let dut = fifo_fst::Dut::create(&mut sim).expect("binds");
    sim.run(async move {
        clock(dut.clk, ns(10)).start();
        dut.rst.set(1);
        dut.in_valid.set(0);
        dut.out_ready.set(0);
        dut.clk.falling().await;
        dut.rst.set(0);
        for beat in 0..6u32 {
            dut.in_valid.set(1);
            dut.in_data.set(u128::from(beat));
            dut.clk.falling().await;
        }
        dut.in_valid.set(0);
        ReadOnly::new().await;
        assert_eq!(dut.in_ready.get(), Bit::ZERO, "4 deep, so full");
    })
    .expect("passes");
    let steps = timesteps(&sim);
    drop(sim);
    let fst = fst::read(&path);
    assert_eq!(fst.times, steps);
    assert!(fst.vars >= 9, "{fst:?}");
}
