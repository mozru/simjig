//! Waves on failure, end to end: a test of this binary runs in a child process with the
//! waves variables set, the harness replays it (a grandchild), and the run directory gets
//! `waves.fst`, `waves.gtkw` and `replay/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use simjig::{
    DutModel, Failure, FakeModel, ModelOptions, Precision, RawSig, ReadOnly, Sim, Timer, clock, ns,
};

use crate::counter_fst;
use crate::fst;

/// Fails in the `ReadOnly` of the 300th rising edge after reset, at t = 3000 ns.
#[simjig::test(timeout = "10us")]
#[ignore = "fails on purpose; run by the tests below in a child process"]
async fn counter_overflows(dut: counter_fst::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(1);
    dut.clk.falling().await;
    dut.rst.set(0);
    for _ in 0..300 {
        dut.clk.rising().await;
    }
    ReadOnly::new().await;
    let count = dut.count.get().to_u128();
    assert_eq!(count, 0, "the counter kept counting");
}

/// Passes after 50 cycles.
#[simjig::test(timeout = "10us")]
#[ignore = "run by the tests below in a child process"]
async fn counter_counts(dut: counter_fst::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(1);
    dut.clk.falling().await;
    dut.rst.set(0);
    for _ in 0..50 {
        dut.clk.rising().await;
    }
}

/// A DUT whose model reads the wall clock: `limit` is the current nanosecond within the
/// second (plus 1), a different value in every process.
#[derive(Clone, Copy)]
struct WallClock {
    limit: RawSig,
}

impl DutModel for WallClock {
    fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
        let mut model = FakeModel::new(Precision::NS);
        let limit = model.signal("limit", 32);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after 1970")
            .subsec_nanos();
        model.comb(move |s| s.set(limit, u64::from(nanos % 1_000_000) + 1));
        Ok(Sim::from_fake(model))
    }

    fn create(sim: &mut Sim) -> Result<Self, Failure> {
        let limit = sim.signal("limit").expect("the model's signal");
        Ok(Self { limit })
    }
}

/// Fails after `limit` ns: a failure time that depends on the wall clock.
#[simjig::test]
#[ignore = "non-deterministic on purpose; run by the tests below in a child process"]
async fn depends_on_the_wall_clock(dut: WallClock) {
    ReadOnly::new().await;
    let limit = dut.limit.read_u64();
    Timer::new(ns(limit)).await;
    panic!("waited {limit} ns");
}

/// Runs `test` of this binary in a child process with `vars` set; its runs go to `runs`.
fn run_child(test: &str, runs: &str, vars: &[(&str, &str)]) -> (Output, PathBuf) {
    let exe = std::env::current_exe().expect("the test binary");
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("waves-runs")
        .join(runs);
    std::fs::remove_dir_all(&root).ok();
    let mut command = Command::new(exe);
    command
        .args(["--ignored", "--exact", test, "--nocapture"])
        .env("SIMJIG_RUNS_DIR", &root);
    for var in [
        "SIMJIG_SEED",
        "SIMJIG_WAVES",
        "SIMJIG_WAVES_ON_FAIL",
        "SIMJIG_TRACE",
        "SIMJIG_TRACE_FROM",
        "SIMJIG_TRACE_TO",
    ] {
        command.env_remove(var);
    }
    command.envs(vars.iter().copied());
    let output = command.output().expect("the child runs");
    let dir = run_dir(&root, test);
    (output, dir)
}

/// The only run directory of `test` under `root`.
fn run_dir(root: &Path, test: &str) -> PathBuf {
    let tests = root
        .join("simjig")
        .join("waves_tests")
        .join(test.replace("::", "__"));
    let mut seeds: Vec<PathBuf> = std::fs::read_dir(&tests)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    !p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(seeds.len() <= 1, "{seeds:?}");
    seeds.pop().unwrap_or(tests)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn summary(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("summary.json")).expect("summary.json");
    serde_json::from_str(&text).expect("valid JSON")
}

/// The primary marker of a `.gtkw` file (the `*<zoom> <marker> ...` line).
fn marker(gtkw: &str) -> u64 {
    let line = gtkw
        .lines()
        .find(|l| l.starts_with('*'))
        .unwrap_or_else(|| panic!("no marker line in:\n{gtkw}"));
    line.split(' ')
        .nth(1)
        .and_then(|t| t.parse().ok())
        .expect("a marker time")
}

#[test]
fn a_failure_with_waves_on_fail_gets_waves_from_a_replay() {
    let test = "on_fail::counter_overflows";
    let (output, dir) = run_child(test, "on-fail", &[("SIMJIG_WAVES_ON_FAIL", "1")]);
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    let fail_time = summary(&dir)["failure"]["time"]["ticks"]
        .as_u64()
        .expect("a failure time");
    assert_eq!(fail_time, 3_000_000, "{err}");

    let fst_path = dir.join("waves.fst");
    assert!(
        err.contains(&format!("waves:    {}", fst_path.display())),
        "the report names the waves:\n{err}"
    );
    assert!(err.contains("replay:   reproduced"), "{err}");
    let waves = fst::read(&fst_path);
    assert_eq!(waves.end, fail_time, "the waves end at the failure");
    // The default window: max(10% of 3000 ns, 1 us) before the failure.
    let from = fail_time - 1_000_000;
    assert_eq!(waves.start, from, "a timestep falls on {from}");
    assert!(waves.times.iter().all(|&t| (from..=fail_time).contains(&t)));

    let gtkw = std::fs::read_to_string(dir.join("waves.gtkw")).expect("waves.gtkw");
    assert_eq!(marker(&gtkw), fail_time);
    assert!(
        gtkw.contains(&format!("[dumpfile] \"{}\"", fst_path.display())),
        "{gtkw}"
    );
    assert!(dir.join("replay/summary.json").is_file());
}

#[test]
fn without_a_waves_variable_nothing_is_replayed() {
    let test = "on_fail::counter_overflows";
    let (output, dir) = run_child(test, "no-waves", &[]);
    let err = stderr(&output);
    assert!(
        err.contains("waves:    SIMJIG_WAVES=1 SIMJIG_SEED="),
        "{err}"
    );
    assert!(!dir.join("waves.fst").exists() && !dir.join("replay").exists());
}

#[test]
fn simjig_waves_window_traces_the_given_span_of_a_passing_run() {
    let test = "on_fail::counter_counts";
    let (output, dir) = run_child(test, "window", &[("SIMJIG_WAVES", "window:100ns..200ns")]);
    let err = stderr(&output);
    assert!(output.status.success(), "{err}");
    assert!(err.contains("replay:   reproduced"), "{err}");
    let waves = fst::read(&dir.join("waves.fst"));
    assert_eq!((waves.start, waves.end), (100_000, 200_000));
    assert!(
        waves
            .times
            .iter()
            .all(|&t| (100_000..=200_000).contains(&t))
    );
}

#[test]
fn a_replay_that_ends_differently_is_reported_as_non_determinism() {
    let test = "on_fail::depends_on_the_wall_clock";
    let (output, dir) = run_child(test, "nondet", &[("SIMJIG_WAVES_ON_FAIL", "1")]);
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    assert!(err.contains("replay:   NON-DETERMINISTIC"), "{err}");
    assert!(
        err.contains("time:     t="),
        "the differing time is named:\n{err}"
    );
    assert!(
        err.contains("waves:    none written"),
        "a FakeModel cannot trace:\n{err}"
    );
    let replay = summary(&dir.join("replay"));
    assert_ne!(
        replay["failure"]["time"]["ticks"],
        summary(&dir)["failure"]["time"]["ticks"]
    );
}
