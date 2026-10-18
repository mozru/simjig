//! The runner without the macro: seeds, determinism, timeouts, setup failures and the run
//! directory, through `simjig::__private::run` with an explicit environment.

use std::path::PathBuf;

use serde_json::Value;
use simjig::__private::{RunEnv, TestMeta, TestOutcome, Timeout, TimeoutArg, run};
use simjig::{
    DutModel, Failure, FakeModel, ModelOptions, Precision, Seed, SeedSource, Sim, SimDuration,
    SimTime, TestCtx, TimeUnit, rising, yield_now,
};

use crate::counter;

fn meta(name: &'static str) -> TestMeta {
    TestMeta {
        module_path: "harness_tests::runner",
        name,
        package: "simjig",
        timeout: TimeoutArg::Default,
        ignored: false,
        should_panic: false,
    }
}

/// A run environment writing under `$CARGO_TARGET_TMPDIR/harness/<dir>`, emptied first so a
/// run directory left by an earlier run cannot pass for this one's.
fn env(seed: Option<u64>, dir: &str) -> RunEnv {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("harness")
        .join(dir);
    std::fs::remove_dir_all(&root).ok(); // absent on a first run
    RunEnv {
        seed,
        runs_root: Some(root),
        launcher: None,
        binary: Some("harness_tests".to_owned()),
        ..RunEnv::default()
    }
}

/// 20 cycles of random data from the stream `"stim"`.
async fn random_stimulus(ctx: TestCtx, dut: counter::Dut) {
    let mut rng = ctx.rng("stim");
    dut.en.write_u64(1);
    for _ in 0..20 {
        dut.data.write_u64(rng.below(256));
        rising(dut.clk).await;
    }
}

fn run_stimulus(name: &'static str, seed: Option<u64>, dir: &str) -> TestOutcome {
    run::<counter::Dut, _, _>(&meta(name), &env(seed, dir), random_stimulus)
}

#[test]
fn the_same_seed_gives_the_same_trace_hash() {
    let a = run_stimulus("det", Some(7), "det-a");
    let b = run_stimulus("det", Some(7), "det-b");
    let c = run_stimulus("det", Some(8), "det-c");
    assert!(a.failure.is_none(), "{:?}", a.failure);
    assert!(a.trace_hash.is_some());
    assert_eq!(a.trace_hash, b.trace_hash);
    assert_ne!(
        a.trace_hash, c.trace_hash,
        "the stimulus depends on the seed"
    );
}

#[test]
fn the_default_seed_comes_from_the_test_name() {
    let a = run_stimulus("named", None, "named-a");
    let b = run_stimulus("named", None, "named-b");
    let other = run_stimulus("other", None, "named-c");
    assert_eq!(a.test, "runner::named");
    assert_eq!(a.seed, Seed::from_test_name("simjig", "runner::named"));
    assert_eq!(a.seed.source(), SeedSource::TestName);
    assert_eq!((a.seed, a.trace_hash), (b.seed, b.trace_hash));
    assert_ne!(a.seed, other.seed);
}

#[test]
fn an_env_seed_overrides_the_test_name_seed() {
    let a = run_stimulus("env", Some(0x1234), "env");
    assert_eq!(a.seed, Seed::new(0x1234, SeedSource::Env));
}

#[test]
fn a_timeout_becomes_the_time_limit() {
    let mut meta = meta("timeout");
    meta.timeout = TimeoutArg::Given(Timeout {
        value: 100,
        unit: TimeUnit::Ns,
    });
    let outcome = run::<counter::Dut, _, _>(&meta, &env(None, "timeout"), |_, dut| forever(dut));
    let report = outcome.failure.expect("the test times out");
    assert_eq!(report.kind, "TimeLimit");
    assert!(
        report
            .details
            .contains(&"timeout:  100 ns (from `#[simjig::test(timeout = ...)]`)".to_owned()),
        "{report}"
    );
    assert_eq!(outcome.end_time.map(SimTime::ticks), Some(100));
}

#[test]
#[should_panic(expected = "is not a whole number of ticks")]
fn a_timeout_finer_than_the_precision_is_rejected() {
    let mut meta = meta("fine");
    meta.timeout = TimeoutArg::Given(Timeout {
        value: 1,
        unit: TimeUnit::Fs,
    });
    run::<counter::Dut, _, _>(&meta, &env(None, "fine"), |_, _| async {});
}

/// Runs 20 ms, past the default timeout, but ends: if the default did not fire, the test
/// fails on its own assertion rather than hanging until nextest kills it.
async fn tick_20ms() {
    for _ in 0..20 {
        simjig::Timer::new(simjig::ms(1)).await;
    }
}

#[test]
fn a_test_without_a_timeout_stops_at_the_default() {
    let outcome = run::<(), _, _>(&meta("default"), &env(None, "default"), |_, ()| tick_20ms());
    let report = outcome.failure.expect("the default timeout fires");
    assert_eq!(report.kind, "TimeLimit");
    let line = report
        .details
        .iter()
        .find(|l| l.starts_with("timeout:"))
        .unwrap_or_else(|| panic!("no timeout line in\n{report}"));
    assert!(
        line.contains("the default") && line.contains("timeout = \"none\""),
        "{line}"
    );
    // An empty model runs at 1 ps; the default is 10 ms.
    assert_eq!(outcome.end_time.map(SimTime::ticks), Some(10_000_000_000));
    let dir = outcome.run_dir.expect("written");
    let summary = read_json(&dir.join("summary.json"));
    assert_eq!(summary["timeout"]["source"], "default");
}

/// An empty model at the precision `10^EXP` s.
struct AtPrecision<const EXP: i8>;

impl<const EXP: i8> DutModel for AtPrecision<EXP> {
    fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
        let precision = Precision::from_exponent(EXP).expect("in range");
        Ok(Sim::from_fake(FakeModel::new(precision)))
    }

    fn create(_sim: &mut Sim) -> Result<Self, Failure> {
        Ok(Self)
    }
}

/// Waits one tick at a time, for 20 ticks: longer than any default timeout below.
async fn twenty_ticks() {
    for _ in 0..20 {
        simjig::Timer::new(SimDuration::from_ticks(1)).await;
    }
}

/// The default timeout's end time, in ticks, at the precision `10^EXP` s.
fn default_timeout_ticks<const EXP: i8>() -> Option<u64> {
    let name = if EXP == 0 { "coarse_s" } else { "coarse_10ms" };
    let outcome =
        run::<AtPrecision<EXP>, _, _>(&meta(name), &env(None, name), |_, _| twenty_ticks());
    let report = outcome.failure.expect("the default timeout fires");
    assert_eq!(report.kind, "TimeLimit");
    outcome.end_time.map(SimTime::ticks)
}

#[test]
fn the_default_timeout_is_10ms_rounded_up_to_whole_ticks() {
    // 1 s and 10 ms per tick: 10 ms rounds up to one tick, not to ten (10 s and 100 ms).
    assert_eq!(default_timeout_ticks::<0>(), Some(1));
    assert_eq!(default_timeout_ticks::<-2>(), Some(1));
}

async fn run_20ms() {
    simjig::Timer::new(simjig::ms(20)).await;
}

#[test]
fn timeout_none_disables_the_default() {
    let mut meta = meta("none");
    meta.timeout = TimeoutArg::Disabled;
    let outcome = run::<(), _, _>(&meta, &env(None, "none"), |_, ()| run_20ms());
    assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
    let summary = read_json(&outcome.run_dir.expect("written").join("summary.json"));
    assert_eq!(summary["timeout"], Value::Null);
}

async fn forever(dut: counter::Dut) {
    loop {
        rising(dut.clk).await;
    }
}

/// The line of the `panic!` in [`panics`].
const PANIC_LINE: u32 = line!() + 4;

async fn panics() {
    yield_now().await;
    panic!("value {} out of range", 42);
}

#[test]
fn a_task_panic_reports_its_source_location() {
    let outcome = run::<(), _, _>(&meta("panics"), &env(None, "panics"), |_, ()| panics());
    let report = outcome.failure.expect("the body panics");
    assert_eq!(report.kind, "TaskPanic");
    let location = report.location.as_deref().unwrap_or_default();
    assert!(
        location.ends_with(&format!("harness_tests/runner.rs:{PANIC_LINE}:5")),
        "{location}"
    );
    assert_eq!(report.task.as_deref(), Some("main"));
}

/// Panics with the same message as [`main_panics_after_detaching`], at another line.
struct PanicsOnDrop;

impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        panic!("boom");
    }
}

/// The line of the `panic!` in [`main_panics_after_detaching`].
const MAIN_PANIC_LINE: u32 = line!() + 9;

async fn main_panics_after_detaching() {
    simjig::spawn("b", async {
        let _guard = PanicsOnDrop;
        std::future::pending::<()>().await;
    })
    .detach();
    yield_now().await;
    panic!("boom");
}

#[test]
fn a_later_panic_with_the_same_message_does_not_move_the_location() {
    // `main` fails first; cancelling `b` then panics "boom" again, in its `Drop`. The
    // report must keep the kernel's location of the first panic.
    let outcome = run::<(), _, _>(&meta("drop_boom"), &env(None, "drop_boom"), |_, ()| {
        main_panics_after_detaching()
    });
    let report = outcome.failure.expect("main panics");
    assert_eq!(report.task.as_deref(), Some("main"));
    let location = report.location.as_deref().unwrap_or_default();
    assert!(
        location.ends_with(&format!("harness_tests/runner.rs:{MAIN_PANIC_LINE}:5")),
        "{location}"
    );
}

/// A DUT whose model has none of the signals it looks for.
struct NoSignals;

impl DutModel for NoSignals {
    fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
        Ok(Sim::from_fake(FakeModel::new(Precision::NS)))
    }

    fn create(sim: &mut Sim) -> Result<Self, Failure> {
        Err(counter::setup_failure(
            sim,
            "the model has no signal `clk`".into(),
        ))
    }
}

/// A DUT whose model cannot be built.
struct NoModel;

impl DutModel for NoModel {
    fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
        let sim = Sim::from_fake(FakeModel::new(Precision::NS));
        Err(counter::setup_failure(
            &sim,
            "libVfifo.so: not found".into(),
        ))
    }

    fn create(_sim: &mut Sim) -> Result<Self, Failure> {
        unreachable!("instantiate fails")
    }
}

async fn never() {
    yield_now().await;
    unreachable!("the body must not run");
}

#[test]
fn setup_failures_are_reported_without_running_the_body() {
    let created = run::<NoSignals, _, _>(&meta("create"), &env(None, "create"), |_, _| never());
    let report = created.failure.expect("create fails");
    assert_eq!(report.kind, "ModelFatal");
    assert_eq!(report.message, "the model has no signal `clk`");
    assert!(created.trace_hash.is_some(), "the model was built");

    let built = run::<NoModel, _, _>(&meta("build"), &env(None, "build"), |_, _| never());
    assert_eq!(built.failure.expect("instantiate fails").kind, "ModelFatal");
    assert_eq!(built.trace_hash, None);
    let dir = built.run_dir.expect("written");
    assert!(!dir.join("events.jsonl").exists(), "no model, no events");
    let summary = read_json(&dir.join("summary.json"));
    assert_eq!(summary["precision"], Value::Null);
    assert_eq!(summary["events"], Value::Null);
}

fn read_json(path: &std::path::Path) -> Value {
    let text = std::fs::read_to_string(path).expect("readable");
    serde_json::from_str(&text).expect("valid JSON")
}

async fn accumulate_then_panic(dut: counter::Dut) {
    dut.en.write_u64(1);
    dut.data.write_u64(1);
    simjig::cycles(dut.clk, 3).await;
    panic!("acc is {}", dut.acc.read_u64());
}

#[test]
fn the_run_dir_holds_the_summary_and_the_events() {
    let outcome = run::<counter::Dut, _, _>(&meta("dir"), &env(Some(0xabc), "dir"), |_, dut| {
        accumulate_then_panic(dut)
    });
    let report = outcome.failure.expect("the body panics");
    let dir = outcome.run_dir.expect("written");
    assert!(
        dir.ends_with("simjig/harness_tests/runner__dir/0000000000000abc"),
        "{}",
        dir.display()
    );
    assert_eq!(report.run_dir.as_ref(), Some(&dir));

    let summary = read_json(&dir.join("summary.json"));
    assert_eq!(summary["schema"], "simjig.run/1");
    assert_eq!(summary["test"], "runner::dir");
    assert_eq!(summary["package"], "simjig");
    assert_eq!(summary["status"], "fail");
    assert_eq!(summary["seed"], "0x0000000000000abc");
    assert_eq!(summary["seed_source"], "env");
    assert_eq!(
        summary["verilator_seed"],
        u64::from(Seed::new(0xabc, SeedSource::Env).verilator_seed())
    );
    assert_eq!(summary["precision"], "1ns");
    assert_eq!(
        summary["end_time"]["ticks"],
        outcome.end_time.map(SimTime::ticks).unwrap_or_default()
    );
    assert_eq!(summary["timeout"]["source"], "default");
    let hash = format!("{:#018x}", outcome.trace_hash.unwrap_or_default());
    assert_eq!(summary["trace_hash"], hash.as_str());
    let failure = &summary["failure"];
    assert_eq!(failure["kind"], "TaskPanic");
    assert_eq!(failure["signature"], report.signature.to_string().as_str());
    assert_eq!(failure["task"], "main");
    assert_eq!(failure["repro"], report.run.repro().as_str());
    assert_eq!(failure["report"], report.to_string().as_str());

    let events = std::fs::read_to_string(dir.join("events.jsonl")).expect("readable");
    let lines: Vec<Value> = events
        .lines()
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect();
    assert_eq!(summary["events"]["written"], lines.len());
    assert_eq!(summary["events"]["file"], "events.jsonl");
    assert!(
        lines
            .iter()
            .any(|e| e["kind"] == "Poll" && e["task"].is_u64())
    );
    // The run stops at the failure; only the cancellation of the remaining tasks follows.
    let failure_at = lines.iter().position(|e| e["kind"] == "Failure");
    let after: Vec<&Value> = lines[failure_at.expect("a Failure event") + 1..]
        .iter()
        .map(|e| &e["kind"])
        .collect();
    assert!(
        after.iter().all(|k| *k == "Cancel" || *k == "Done"),
        "{after:?}"
    );
}

#[test]
fn a_passing_run_writes_a_passing_summary() {
    let outcome = run_stimulus("pass", None, "pass");
    let summary = read_json(&outcome.run_dir.expect("written").join("summary.json"));
    assert_eq!(summary["status"], "pass");
    assert_eq!(summary["failure"], Value::Null);
    assert_eq!(summary["seed_source"], "test-name");
}

/// Compares a random byte with a fixed one, the way a scoreboard assertion would.
async fn compare_bits(ctx: TestCtx) {
    yield_now().await;
    let got = simjig_bits::Bits::<8>::new_truncating(u128::from(ctx.rng("stim").below(200)));
    let want = simjig_bits::Bits::<8>::new_truncating(0xff);
    assert_eq!(got, want, "beat {} mismatched", ctx.seed().value() % 7);
}

#[test]
fn bits_values_do_not_change_the_signature() {
    let report = |seed| {
        run::<(), _, _>(&meta("bits"), &env(Some(seed), "bits"), |ctx, ()| {
            compare_bits(ctx)
        })
        .failure
        .expect("never 0xff")
    };
    let (a, b) = (report(1), report(2));
    assert_ne!(a.message, b.message, "the values differ");
    assert!(a.message.contains("8'h"), "{}", a.message);
    assert_eq!(a.signature, b.signature, "{}\n{}", a.message, b.message);
}

/// The lines of the two `rng("stim")` calls in [`reuses_a_stream`].
const FIRST_RNG_LINE: u32 = line!() + 4;
const SECOND_RNG_LINE: u32 = line!() + 6;

async fn reuses_a_stream(ctx: TestCtx) {
    let mut a = ctx.rng("stim");
    let _other = ctx.rng("ready");
    yield_now().await;
    let mut b = ctx.rng("stim");
    // Without the check, `b` silently repeats `a`'s values.
    assert_ne!(a.next_u64(), b.next_u64());
}

#[test]
fn reusing_a_stream_path_fails_at_the_second_call() {
    let outcome = run::<(), _, _>(&meta("reuse"), &env(None, "reuse"), |ctx, ()| {
        reuses_a_stream(ctx)
    });
    let report = outcome.failure.expect("the second rng(\"stim\") panics");
    assert_eq!(report.kind, "TaskPanic");
    let location = report.location.as_deref().unwrap_or_default();
    assert!(
        location.ends_with(&format!("harness_tests/runner.rs:{SECOND_RNG_LINE}:21")),
        "{location}"
    );
    assert!(
        report
            .message
            .contains("the random stream \"stim\" was already taken")
            && report
                .message
                .contains(&format!("harness_tests/runner.rs:{FIRST_RNG_LINE}:21")),
        "{}",
        report.message
    );
}

#[test]
fn a_test_whose_models_cannot_trace_offers_no_waves_command() {
    let outcome = run::<(), _, _>(&meta("panics"), &env(None, "no-trace"), |_, ()| panics());
    let text = outcome.failure.expect("the body panics").to_string();
    let line = text
        .lines()
        .find(|l| l.starts_with("  waves:"))
        .unwrap_or_else(|| panic!("no waves line:\n{text}"));
    assert_eq!(
        line,
        "  waves:    none: no model of this test can trace (a FakeModel, or a Verilator model \
         built without `.trace_fst(true)` on its `simjig_build::Dut`)"
    );
}
