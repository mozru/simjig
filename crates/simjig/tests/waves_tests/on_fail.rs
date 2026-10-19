//! Waves on failure, end to end: a test of this binary runs in a child process with the
//! waves variables set, the harness replays it (a grandchild), and the run directory gets
//! `waves.fst`, `waves.gtkw` and `replay/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use simjig::fork;
use simjig::{
    DutModel, Failure, FakeModel, ModelOptions, Precision, RawSig, ReadOnly, Sim, TestCtx, Timer,
    clock, ns,
};

use crate::fst;
use crate::{counter_fst, fifo_fst};

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

/// `counter_overflows` as a synchronous test: fails after the 300th cycle, at t = 3000 ns.
#[simjig::test(sync, timeout = "10us")]
#[ignore = "fails on purpose; run by the tests below in a child process"]
fn counter_overflows_sync(dut: counter_fst::Dut) {
    let mut cyc = simjig::SyncClock::new(dut.clk, ns(10));
    dut.rst.set(1);
    dut.en.set(1);
    cyc.tick();
    dut.rst.set(0);
    cyc.ticks(299);
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

/// Two traced DUTs, both clocked; fails at t = 3000 ns like `counter_overflows`.
#[simjig::test(timeout = "10us")]
#[ignore = "fails on purpose; run by the tests below in a child process"]
async fn two_duts_overflow(counter: counter_fst::Dut, fifo: fifo_fst::Dut) {
    clock(counter.clk, ns(10)).start();
    clock(fifo.clk, ns(10)).start();
    counter.rst.set(1);
    counter.en.set(1);
    fifo.rst.set(1);
    counter.clk.falling().await;
    counter.rst.set(0);
    fifo.rst.set(0);
    fifo.in_valid.set(1);
    fifo.out_ready.set(1);
    for _ in 0..300 {
        counter.clk.rising().await;
    }
    ReadOnly::new().await;
    assert_eq!(
        counter.count.get().to_u128(),
        0,
        "the counter kept counting"
    );
}

/// Forks two branches after 100 cycles (t = 1000 ns): branch 0 passes, branch 1 fails 50
/// cycles later (t = 1500 ns), so the test fails at the checkpoint.
#[simjig::test(timeout = "10us")]
#[ignore = "fails on purpose; run by the tests below in a child process"]
async fn a_branch_overflows(ctx: &TestCtx, dut: counter_fst::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(1);
    dut.clk.falling().await;
    dut.rst.set(0);
    for _ in 0..100 {
        dut.clk.rising().await;
    }
    let branches = fork::run_from_checkpoint(ctx.seed(), 2, |branch| async move {
        for _ in 0..5 + 45 * branch.index() {
            dut.clk.rising().await;
        }
        ReadOnly::new().await;
        assert_eq!(branch.index(), 0, "branch 1 fails");
        Ok(())
    })
    .await
    .expect("forks");
    branches.into_values();
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

/// A synchronous test gets waves on failure the same way: the replay (traced, so not
/// held) reproduces the failure, and the waves end at it, with a sample at every edge.
/// (What each sample holds is checked against the model's dumps in `model::verilator`.)
#[test]
fn a_failing_sync_test_gets_waves_from_a_replay() {
    let test = "on_fail::counter_overflows_sync";
    let (output, dir) = run_child(test, "on-fail-sync", &[("SIMJIG_WAVES_ON_FAIL", "1")]);
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    let fail_time = summary(&dir)["failure"]["time"]["ticks"]
        .as_u64()
        .expect("a failure time");
    assert_eq!(fail_time, 3_000_000, "{err}");
    assert!(err.contains("replay:   reproduced"), "{err}");
    assert!(err.contains("repro:"), "{err}");
    // The header gives the cycle the SyncClock reached: 1 + 299 rising edges.
    assert!(err.contains("t=3 us (cycle 300)"), "{err}");
    let waves = fst::read(&dir.join("waves.fst"));
    assert_eq!(waves.end, fail_time, "the waves end at the failure");
    // One sample per edge (each half period is a timestep with one eval, dumped right
    // after it), every one in the default window of 1 us before the failure.
    let from = fail_time - 1_000_000;
    let edges: Vec<u64> = (from..=fail_time).step_by(5_000).collect();
    assert_eq!(waves.times, edges);
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

/// `SIMJIG_WAVES` asks for this run's replay, so it replays even a run no model of which
/// can trace: the replay is still the determinism check.
#[test]
fn a_replay_that_ends_differently_is_reported_as_non_determinism() {
    let test = "on_fail::depends_on_the_wall_clock";
    let (output, dir) = run_child(test, "nondet", &[("SIMJIG_WAVES", "1")]);
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

/// `SIMJIG_WAVES_ON_FAIL` (set for a whole CI run) replays a failure only for its waves:
/// a run no model of which can trace is not replayed, and the report says why it has none.
#[test]
fn waves_on_fail_does_not_replay_a_run_that_cannot_trace() {
    let test = "on_fail::depends_on_the_wall_clock";
    let (output, dir) = run_child(test, "no-trace", &[("SIMJIG_WAVES_ON_FAIL", "1")]);
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    assert!(
        err.contains("waves:    none: no model of this test can trace"),
        "{err}"
    );
    assert!(!err.contains("replay:"), "{err}");
    assert!(dir.join("summary.json").is_file(), "{err}");
    assert!(!dir.join("replay").exists(), "nothing was replayed");
}

/// The `.gtkw` file's `[dumpfile]`.
fn dumpfile(gtkw: &Path) -> PathBuf {
    let text = std::fs::read_to_string(gtkw).unwrap_or_else(|e| panic!("{}: {e}", gtkw.display()));
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("[dumpfile] "))
        .unwrap_or_else(|| panic!("no [dumpfile] in {}", gtkw.display()));
    PathBuf::from(line.trim_matches('"'))
}

#[test]
fn every_model_of_a_failed_multi_dut_test_gets_its_waves() {
    let test = "on_fail::two_duts_overflow";
    let (output, dir) = run_child(test, "two-duts", &[("SIMJIG_WAVES_ON_FAIL", "1")]);
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    assert!(err.contains("replay:   reproduced"), "{err}");
    for (fst_name, gtkw_name) in [
        ("waves.fst", "waves.gtkw"),
        ("waves.model-1.fst", "waves.model-1.gtkw"),
    ] {
        let fst_path = dir.join(fst_name);
        let waves = fst::read(&fst_path);
        assert_eq!(waves.end, 3_000_000, "{fst_name} ends at the failure");
        assert!(
            err.contains(&fst_path.display().to_string()),
            "the report names {fst_name}:\n{err}"
        );
        let gtkw = dir.join(gtkw_name);
        assert_eq!(dumpfile(&gtkw), fst_path, "{gtkw_name}");
        assert!(
            err.contains(&format!("gtkwave {}", gtkw.display())),
            "the report names {gtkw_name}:\n{err}"
        );
    }
}

/// The report of a test failed by a fork branch: its header gives the branch's failure
/// time and labels the checkpoint's, and the help is said once (the branch's, in the
/// message), not again for the parent's panic.
#[test]
fn a_failing_fork_branch_is_reported_at_its_own_time() {
    let test = "on_fail::a_branch_overflows";
    let (output, _) = run_child(test, "branch-report", &[]);
    let err = stderr(&output);
    let report = &err[err
        .find("FAIL on_fail::")
        .unwrap_or_else(|| panic!("{err}"))..];
    let header = report.lines().next().expect("a header");
    assert!(
        header.contains("  t=1500 ns in fork branch 1 (checkpoint t=1 us)  [TaskPanic"),
        "{header}"
    );
    assert_eq!(report.matches("help:").count(), 1, "{report}");
}

#[test]
fn a_failing_fork_branch_gets_the_waves_of_its_own_span() {
    let test = "on_fail::a_branch_overflows";
    let (output, dir) = run_child(test, "branch", &[("SIMJIG_WAVES_ON_FAIL", "1")]);
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    assert!(err.contains("replay:   reproduced"), "{err}");
    // The checkpoint is at the 100th rising edge after the falling edge at 5 ns.
    let checkpoint = 1_000_000;
    let failed_at = checkpoint + 50 * 10_000;
    assert_eq!(
        summary(&dir)["failure"]["time"]["ticks"].as_u64(),
        Some(checkpoint),
        "{err}"
    );

    let branch = dir.join("waves.branch-1.fst");
    let waves = fst::read(&branch);
    assert_eq!(
        waves.end, failed_at,
        "the branch's waves go on past the checkpoint, to its failure"
    );
    let waves_line = err
        .lines()
        .find(|l| l.trim_start().starts_with("waves:"))
        .unwrap_or_else(|| panic!("no waves line:\n{err}"));
    assert!(
        waves_line.contains(&branch.display().to_string()),
        "the waves line names the failing branch's file:\n{err}"
    );
    let gtkw_path = dir.join("waves.branch-1.gtkw");
    assert_eq!(dumpfile(&gtkw_path), branch);
    let gtkw = std::fs::read_to_string(&gtkw_path).expect("the branch's .gtkw");
    assert_eq!(
        marker(&gtkw),
        failed_at,
        "the marker is at the branch's failure"
    );
    for other in ["waves.fst", "waves.branch-0.fst"] {
        fst::read(&dir.join(other));
        assert!(
            err.contains(&dir.join(other).display().to_string()),
            "the report names {other}:\n{err}"
        );
    }
}

/// Forks two branches after 100 cycles (t = 1000 ns): branch 0 passes, branch 1 aborts 50
/// cycles later, so its child dies by `SIGABRT` without a report or a finished trace.
#[simjig::test(timeout = "10us")]
#[ignore = "fails on purpose; run by the tests below in a child process"]
async fn a_branch_aborts(ctx: &TestCtx, dut: counter_fst::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(1);
    dut.clk.falling().await;
    dut.rst.set(0);
    for _ in 0..100 {
        dut.clk.rising().await;
    }
    let branches = fork::run_from_checkpoint(ctx.seed(), 2, |branch| async move {
        for _ in 0..5 + 45 * branch.index() {
            dut.clk.rising().await;
        }
        if branch.index() == 1 {
            std::process::abort();
        }
        Ok(())
    })
    .await
    .expect("forks");
    branches.into_values();
}

/// A branch killed by a signal has no failure time: its file is labelled as the dead
/// child's, incomplete, listed after the readable files, with no marker at the checkpoint
/// (where nothing failed) and no `gtkwave` command for a file without an FST header.
#[test]
fn a_fork_branch_killed_by_a_signal_is_listed_last_as_incomplete() {
    let test = "on_fail::a_branch_aborts";
    let (output, dir) = run_child(test, "branch-abort", &[("SIMJIG_WAVES_ON_FAIL", "1")]);
    let err = stderr(&output);
    assert!(!output.status.success(), "{err}");
    assert!(err.contains("replay:   reproduced"), "{err}");
    let report = &err[err.find("  waves:").unwrap_or_else(|| panic!("{err}"))..];
    let waves: Vec<&str> = report
        .lines()
        .take_while(|l| !l.trim_start().starts_with("replay:"))
        .collect();
    let died = dir.join("waves.branch-1.fst");
    assert_eq!(waves.len(), 3, "{err}");
    assert!(
        waves[0].contains(&dir.join("waves.fst").display().to_string()),
        "a readable file comes first:\n{err}"
    );
    let last = waves[2];
    assert!(
        last.contains(&format!(
            "{} (fork branch 1, died (killed by signal 6); file incomplete)",
            died.display()
        )),
        "the dead branch's file comes last, labelled:\n{err}"
    );
    assert!(
        !err.contains("failed at"),
        "nothing failed at the checkpoint:\n{err}"
    );
    let gtkw = dir.join("waves.branch-1.gtkw");
    let bytes = std::fs::read(&died).expect("the dead branch's file exists");
    if bytes.first() == Some(&0) {
        let save = std::fs::read_to_string(&gtkw).expect("a .gtkw for a file with a header");
        let marker = save
            .lines()
            .find(|l| l.starts_with('*'))
            .and_then(|l| l.split(' ').nth(1));
        assert_eq!(marker, Some("-1"), "no marker:\n{save}");
    } else {
        assert!(
            !last.contains("gtkwave"),
            "no header, no viewer command:\n{err}"
        );
        assert!(!gtkw.exists(), "no save file for a file without a header");
    }
}
