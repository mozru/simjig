//! Running one `#[simjig::test]`: seed, model, timeout, body, report, run directory.
//!
//! [`run`] does the work and returns a [`TestOutcome`]; [`run_test`], which the macro calls,
//! reads the environment, runs, and turns a failure into a test failure. Keeping [`run`]
//! free of process state (environment, panics) lets the harness's own tests drive it.
//!
//! A `TaskPanic`'s source location comes from the kernel (`kernel::panics` captures it
//! for the exact poll or drop that panicked); the harness installs no panic hook of its
//! own.

use std::future::Future;
use std::panic;
use std::path::{Path, PathBuf};

use crate::model::trace::TraceOptions;
use crate::{Failure, Precision, Sim, SimDuration, SimTime, TimeUnit};

use super::ctx::TestCtx;
use super::dut::{DutModel, InitValues, ModelOptions};
use super::replay::{self, Original, TRACE_FROM_VAR, TRACE_TO_VAR, TRACE_VAR};
use super::report::{AppliedTimeout, FailureReport, RunIdentity};
use super::rundir::{self, Json, Summary};
use super::seed::{Seed, SeedSource, parse_seed};
use super::waves::{RunEnd, WavesReplay, WavesRequest, WavesWhen};

/// A test's simulation-time budget, as written in `#[simjig::test(timeout = "2ms")]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeout {
    /// The amount.
    pub value: u64,
    /// Its unit.
    pub unit: TimeUnit,
}

/// The `timeout` argument of `#[simjig::test]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutArg {
    /// No `timeout` argument: [`DEFAULT_TIMEOUT_MS`] applies.
    Default,
    /// `timeout = "none"`: no time limit.
    Disabled,
    /// `timeout = "<duration>"`.
    Given(Timeout),
}

/// The simulation-time budget of a `#[simjig::test]` without a `timeout` argument, in
/// milliseconds.
///
/// Without one, a test whose clock keeps running while a task waits forever never ends.
/// 10 ms is 10^6 cycles of a 100 MHz clock: more than unit-level tests need, and still
/// seconds of wall time. Longer tests say `timeout = "..."` or `timeout = "none"`; a
/// test that hangs in wall time without advancing simulation time is nextest's
/// `slow-timeout` to catch.
pub const DEFAULT_TIMEOUT_MS: u64 = 10;

/// What `#[simjig::test]` knows about the test at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestMeta {
    /// `module_path!()` at the test.
    pub module_path: &'static str,
    /// The test function's name.
    pub name: &'static str,
    /// `CARGO_PKG_NAME` of the crate holding the test.
    pub package: &'static str,
    /// The `timeout` argument.
    pub timeout: TimeoutArg,
    /// Whether the test has an `#[ignore]` attribute.
    pub ignored: bool,
    /// Whether the test has a `#[should_panic]` attribute: its failure is the expected
    /// outcome, so the report is not printed (libtest shows the payload if the panic
    /// does not match).
    pub should_panic: bool,
    /// The `init` argument (`init = random`); `None` without one, when `SIMJIG_INIT`
    /// decides.
    pub init: Option<InitValues>,
    /// Whether the run keeps a trace hash (`true` unless `trace_hash = false`).
    pub trace_hash: bool,
}

impl TestMeta {
    /// The libtest name: the module path without the crate name, then the function.
    #[must_use]
    pub fn test_path(&self) -> String {
        match self.module_path.split_once("::") {
            Some((_crate, module)) => format!("{module}::{}", self.name),
            None => self.name.to_owned(),
        }
    }
}

/// The process environment a run depends on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunEnv {
    /// The root seed to use instead of the test-name seed (`SIMJIG_SEED`).
    pub seed: Option<u64>,
    /// The initial values of tests without an `init` argument (`SIMJIG_INIT`).
    pub init: Option<InitValues>,
    /// Where run directories go (`SIMJIG_RUNS_DIR`, else `<target>/simjig/runs`); `None`
    /// writes none.
    pub runs_root: Option<PathBuf>,
    /// The command prefix the repro line needs (`pixi run`), detected by `RunEnv::from_process`.
    pub launcher: Option<String>,
    /// The test binary's name as nextest's `binary()` filter matches it
    /// (`harness_tests`), from the executable's name.
    pub binary: Option<String>,
    /// Whether to replay the run with waves (`SIMJIG_WAVES`, `SIMJIG_WAVES_ON_FAIL`).
    pub waves: Option<WavesRequest>,
    /// Waves to record in this run itself (`SIMJIG_TRACE*`; how a replay is traced). They
    /// are optional: a model that cannot trace runs untraced.
    pub trace: Option<TraceOptions>,
}

impl RunEnv {
    /// Reads `SIMJIG_SEED`, `SIMJIG_INIT`, `SIMJIG_RUNS_DIR`, the waves variables (`SIMJIG_WAVES`,
    /// `SIMJIG_WAVES_ON_FAIL`, `SIMJIG_TRACE*`), and the pixi/conda variables that decide
    /// the repro's launcher (`PIXI_WORKSPACE_ROOT`, `PIXI_PROJECT_ROOT`, `CONDA_PREFIX`,
    /// ...; see `runner::launcher`).
    ///
    /// # Panics
    /// If `SIMJIG_SEED`, `SIMJIG_INIT`, a waves variable or a `SIMJIG_TRACE_*` time is set to something
    /// it does not accept (the message says what is accepted).
    #[must_use]
    pub fn from_process() -> Self {
        let seed = std::env::var("SIMJIG_SEED")
            .ok()
            .map(|text| parse_seed(&text).unwrap_or_else(|e| panic!("{e}")));
        let init = std::env::var("SIMJIG_INIT")
            .ok()
            .map(|text| InitValues::parse(&text).unwrap_or_else(|e| panic!("{e}")));
        let runs_root = std::env::var_os("SIMJIG_RUNS_DIR")
            .map(PathBuf::from)
            .or_else(rundir::default_root);
        let cwd = std::env::current_dir().ok();
        let launcher = launcher(|name| std::env::var(name).ok(), cwd.as_deref());
        let binary = std::env::current_exe()
            .ok()
            .and_then(|exe| rundir::binary_name(&exe));
        let var = |name: &str| std::env::var(name).ok();
        let waves = WavesRequest::from_vars(
            var("SIMJIG_WAVES").as_deref(),
            var("SIMJIG_WAVES_ON_FAIL").as_deref(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let trace = std::env::var_os(TRACE_VAR).map(|path| {
            let ticks = |name: &str| {
                var(name).map(|text| {
                    text.parse::<u64>().map_or_else(
                        |_bad| panic!("{name}={text:?} is not a tick count"),
                        SimTime,
                    )
                })
            };
            let mut trace = TraceOptions::new(path).if_supported();
            if let Some(from) = ticks(TRACE_FROM_VAR) {
                trace = trace.start(from);
            }
            if let Some(to) = ticks(TRACE_TO_VAR) {
                trace = trace.end(to);
            }
            trace
        });
        Self {
            seed,
            init,
            runs_root,
            launcher,
            binary,
            waves,
            trace,
        }
    }
}

/// The prefix a repro command needs to run in the same environment as this test, or
/// `None` for a plain shell.
///
/// A test that runs inside `pixi run` (or `pixi shell`) finds Verilator on `PATH`; the
/// same `cargo` command in a plain shell reruns the build scripts without it and fails
/// with `VerilatorNotFound`. So:
/// - pixi (`PIXI_WORKSPACE_ROOT` or, before pixi 0.40, `PIXI_PROJECT_ROOT`): `pixi run`,
///   with `--manifest-path` when `cwd` (Cargo runs a test in its package directory) is
///   outside the pixi workspace, and `-e <env>` for an environment other than `default`;
/// - a `CONDA_PREFIX` inside a pixi workspace (`<root>/.pixi/envs/<env>`): the same;
/// - any other `CONDA_PREFIX`: `conda run -p <prefix>`.
pub(crate) fn launcher(var: impl Fn(&str) -> Option<String>, cwd: Option<&Path>) -> Option<String> {
    let pixi_root = var("PIXI_WORKSPACE_ROOT").or_else(|| var("PIXI_PROJECT_ROOT"));
    let (root, manifest, env) = if let Some(root) = pixi_root {
        let manifest = var("PIXI_WORKSPACE_MANIFEST").or_else(|| var("PIXI_PROJECT_MANIFEST"));
        (PathBuf::from(root), manifest, var("PIXI_ENVIRONMENT_NAME"))
    } else {
        let prefix = PathBuf::from(var("CONDA_PREFIX")?);
        match pixi_env_of(&prefix) {
            Some((root, env)) => (root, None, Some(env)),
            None => {
                return Some(format!(
                    "conda run -p {}",
                    shell_word(&prefix.to_string_lossy())
                ));
            }
        }
    };
    let mut out = "pixi run".to_owned();
    if !cwd.is_some_and(|cwd| cwd.starts_with(&root)) {
        let manifest = manifest.unwrap_or_else(|| root.to_string_lossy().into_owned());
        out.push_str(" --manifest-path ");
        out.push_str(&shell_word(&manifest));
    }
    if let Some(env) = env.filter(|e| e != "default") {
        out.push_str(" -e ");
        out.push_str(&shell_word(&env));
    }
    Some(out)
}

/// The workspace root and environment name of a pixi environment prefix
/// (`<root>/.pixi/envs/<env>`).
fn pixi_env_of(prefix: &Path) -> Option<(PathBuf, String)> {
    let env = prefix.file_name()?.to_str()?;
    let envs = prefix.parent()?;
    let dot_pixi = envs.parent()?;
    let root = dot_pixi.parent()?;
    let layout = envs.file_name()? == "envs" && dot_pixi.file_name()? == ".pixi";
    layout.then(|| (root.to_path_buf(), env.to_owned()))
}

/// `word`, single-quoted for a POSIX shell if it holds anything but `[A-Za-z0-9/._+-]`.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// What a test body may return: `()` or [`Result`](crate::Result).
#[expect(
    clippy::result_large_err,
    reason = "called once per test, and Failure stays plain data (as Sim::run)"
)]
pub trait TestOutput {
    /// The run's verdict.
    ///
    /// # Errors
    /// The failure the body returned.
    fn into_result(self) -> Result<(), Failure>;
}

impl TestOutput for () {
    fn into_result(self) -> Result<(), Failure> {
        Ok(())
    }
}

impl TestOutput for Result<(), Failure> {
    fn into_result(self) -> Self {
        self
    }
}

/// The result of one test run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestOutcome {
    /// The libtest name.
    pub test: String,
    /// The root seed.
    pub seed: Seed,
    /// The kernel's trace hash; `None` if the model could not be built.
    pub trace_hash: Option<u64>,
    /// The design's precision; `None` if the model could not be built.
    pub precision: Option<Precision>,
    /// Simulation time at the end of the run; `None` if the model could not be built.
    pub end_time: Option<SimTime>,
    /// The failure, if the test failed.
    pub failure: Option<FailureReport>,
    /// The run directory, if one was written.
    pub run_dir: Option<PathBuf>,
    /// Why the run directory could not be written, if it could not.
    pub run_dir_error: Option<String>,
}

/// What [`execute`] leaves behind.
struct Executed {
    sim: Option<Sim>,
    timeout: Option<AppliedTimeout>,
    result: Result<(), Failure>,
}

/// Runs a test: builds `D`, runs `body` as the main task under the test's timeout, and
/// writes the run directory.
///
/// # Panics
/// If the test's `timeout` is not a whole number of ticks of the design's precision.
#[expect(
    clippy::result_large_err,
    reason = "a run returns once, and Failure stays plain data that tests match on"
)]
pub fn run<D, F, Fut>(meta: &TestMeta, env: &RunEnv, body: F) -> TestOutcome
where
    D: DutModel,
    F: FnOnce(TestCtx, D) -> Fut,
    Fut: Future + 'static,
    Fut::Output: TestOutput,
{
    run_with(meta, env, |sim: &mut Sim, ctx, dut| {
        sim.run(body(ctx, dut)).and_then(TestOutput::into_result)
    })
}

/// As [`run`], for a synchronous test (`#[simjig::test(sync)]`): `body` runs through
/// [`Sim::run_sync`].
#[expect(
    clippy::result_large_err,
    reason = "a run returns once, and Failure stays plain data that tests match on"
)]
pub fn run_sync<D, F, R>(meta: &TestMeta, env: &RunEnv, body: F) -> TestOutcome
where
    D: DutModel,
    F: FnOnce(TestCtx, D) -> R,
    R: TestOutput,
{
    run_with(meta, env, |sim: &mut Sim, ctx, dut| {
        sim.run_sync(|| body(ctx, dut))
            .and_then(TestOutput::into_result)
    })
}

/// [`run`] and [`run_sync`]: the seed, options, context and run directory around
/// `go`, which runs the body on the built simulation.
fn run_with<D: DutModel>(
    meta: &TestMeta,
    env: &RunEnv,
    go: impl FnOnce(&mut Sim, TestCtx, D) -> Result<(), Failure>,
) -> TestOutcome {
    // A branch an earlier run on this thread recorded is not this run's.
    crate::fork::forget_failed_branch();
    let test = meta.test_path();
    let seed = env.seed.map_or_else(
        || Seed::from_test_name(meta.package, &test),
        |value| Seed::new(value, SeedSource::Env),
    );
    let init = init_values(meta, env);
    let mut options = ModelOptions::new(seed).with_init(init);
    if let Some(trace) = &env.trace {
        options = options.with_trace(trace.clone());
    }
    let ctx = TestCtx::new(test.clone(), seed, options.verilator_seed);
    let run = RunIdentity {
        test,
        package: meta.package.to_owned(),
        seed,
        init,
        ignored: meta.ignored,
        launcher: env.launcher.clone(),
        binary: env.binary.clone(),
    };
    let executed = execute(meta, &options, ctx, go);
    finish(run, env, &options, executed)
}

/// A test's initial values: its `init` argument, else `SIMJIG_INIT`, else zeros. The
/// argument wins so a test whose design needs zeros keeps them under a `SIMJIG_INIT=random`
/// sweep of the whole suite.
fn init_values(meta: &TestMeta, env: &RunEnv) -> InitValues {
    meta.init.or(env.init).unwrap_or_default()
}

fn execute<D: DutModel>(
    meta: &TestMeta,
    options: &ModelOptions,
    ctx: TestCtx,
    go: impl FnOnce(&mut Sim, TestCtx, D) -> Result<(), Failure>,
) -> Executed {
    let failed = |sim, result| Executed {
        sim,
        timeout: None,
        result,
    };
    let mut sim = match D::instantiate(options) {
        Ok(sim) => sim,
        Err(failure) => return failed(None, Err(failure)),
    };
    let dut = match D::create(&mut sim) {
        Ok(dut) => dut,
        Err(failure) => return failed(Some(sim), Err(failure)),
    };
    if !meta.trace_hash {
        sim.stop_trace_hash();
    }
    let timeout = time_limit(meta.timeout, sim.precision(), ctx.test_name());
    if let Some(timeout) = timeout {
        sim.set_time_limit(SimTime(timeout.duration.ticks()));
    }
    let result = go(&mut sim, ctx, dut);
    Executed {
        sim: Some(sim),
        timeout,
        result,
    }
}

/// The time limit `timeout` asks for at `precision`.
///
/// # Panics
/// If a given timeout is not a whole number of ticks.
fn time_limit(timeout: TimeoutArg, precision: Precision, test: &str) -> Option<AppliedTimeout> {
    match timeout {
        TimeoutArg::Disabled => None,
        TimeoutArg::Given(t) => {
            let duration = precision
                .duration(t.value, t.unit)
                .unwrap_or_else(|e| panic!("#[simjig::test(timeout = ...)] of `{test}`: {e}"));
            Some(AppliedTimeout {
                duration,
                is_default: false,
            })
        }
        TimeoutArg::Default => Some(AppliedTimeout {
            duration: SimDuration::from_ticks(default_timeout_ticks(precision)),
            is_default: true,
        }),
    }
}

/// [`DEFAULT_TIMEOUT_MS`] in ticks of `precision`: `ceil(10 ms / tick)`, at least one.
///
/// At a precision coarser than 1 ms, 10 ms is not always a whole number of ticks; it
/// rounds up rather than failing, since the user wrote no timeout to fix.
fn default_timeout_ticks(precision: Precision) -> u64 {
    // `exponent` is in -15..=0, so neither power overflows.
    let ticks = match precision.exponent() {
        e @ ..=-3 => DEFAULT_TIMEOUT_MS * 10u64.pow(u32::from((-3 - e).unsigned_abs())),
        e => DEFAULT_TIMEOUT_MS.div_ceil(10u64.pow(u32::from((e + 3).unsigned_abs()))),
    };
    ticks.max(1)
}

fn finish(
    run: RunIdentity,
    env: &RunEnv,
    options: &ModelOptions,
    executed: Executed,
) -> TestOutcome {
    let Executed {
        sim,
        timeout,
        result,
    } = executed;
    let dir = env.runs_root.as_ref().map(|root| {
        // `_`: only a harness test driving `run` directly has no binary name.
        let binary = run.binary.as_deref().unwrap_or("_");
        rundir::run_dir(root, &run.package, binary, &run.test, run.seed)
    });
    let mut failure = result.err().map(|failure| {
        let mut report = FailureReport::new(&failure, run.clone(), None, timeout);
        report.run_dir.clone_from(&dir);
        report.can_trace = sim.as_ref().is_some_and(Sim::can_trace);
        report.cycle = sim.as_ref().and_then(Sim::sync_cycle);
        report.failed_branch = crate::fork::take_failed_branch();
        if report.failed_branch.is_some() {
            // The message lists every failed branch with its own help; the parent's
            // (a panic's) would only repeat or contradict them.
            report.help = None;
        }
        report
    });
    let run_dir_error = match &dir {
        Some(dir) => {
            let summary = summary(&run, options, sim.as_ref(), timeout, failure.as_ref());
            write_run_dir(dir, summary, sim.as_ref())
                .err()
                .map(|e| format!("{}: {e}", dir.display()))
        }
        None => Some("no target directory found; set SIMJIG_RUNS_DIR".to_owned()),
    };
    let run_dir = dir.filter(|_| run_dir_error.is_none());
    if let Some(report) = &mut failure {
        report.run_dir.clone_from(&run_dir);
    }
    TestOutcome {
        test: run.test,
        seed: run.seed,
        trace_hash: sim.as_ref().and_then(Sim::full_trace_hash),
        precision: sim.as_ref().map(Sim::precision),
        end_time: sim.as_ref().map(Sim::now),
        failure,
        run_dir,
        run_dir_error,
    }
}

fn summary<'a>(
    run: &'a RunIdentity,
    options: &ModelOptions,
    sim: Option<&Sim>,
    timeout: Option<AppliedTimeout>,
    failure: Option<&'a FailureReport>,
) -> Summary<'a> {
    let precision = sim.map(Sim::precision);
    Summary {
        test: &run.test,
        package: &run.package,
        launcher: run.launcher.as_deref(),
        seed: run.seed,
        verilator_seed: options.verilator_seed,
        init: options.init,
        init_seed: options.random_init(),
        precision: precision.map(|p| p.to_string()),
        end_time: sim.map_or(Json::Null, |s| Json::time(s.now(), s.precision())),
        timeout: match (timeout, precision) {
            (Some(t), Some(p)) => {
                let ticks = t.duration.ticks();
                let source = if t.is_default { "default" } else { "attribute" };
                let mut json = Json::time(SimTime(ticks), p);
                if let Json::Obj(fields) = &mut json {
                    fields.push(("source", Json::str(source)));
                }
                json
            }
            _ => Json::Null,
        },
        trace_hash: sim.and_then(Sim::full_trace_hash),
        events: None,
        failure,
    }
}

fn write_run_dir(dir: &Path, mut summary: Summary<'_>, sim: Option<&Sim>) -> std::io::Result<()> {
    let events = sim.map(Sim::recent_trace);
    summary.events = sim
        .zip(events.as_ref())
        .map(|(s, e)| (e.len(), s.stats().records));
    rundir::write(dir, &summary, events.as_deref())
}

/// The entry point of the code `#[simjig::test]` generates: runs the test with the
/// process environment and fails the calling `#[test]` with the report.
///
/// # Panics
/// If the test failed, with the report as the payload (printed once, on stderr).
pub fn run_test<D, F, Fut>(meta: &TestMeta, body: F)
where
    D: DutModel,
    F: FnOnce(TestCtx, D) -> Fut,
    Fut: Future + 'static,
    Fut::Output: TestOutput,
{
    let env = RunEnv::from_process();
    let outcome = run(meta, &env, body);
    report_test(meta, &env, outcome);
}

/// As [`run_test`], for a synchronous test (`#[simjig::test(sync)]`).
///
/// # Panics
/// If the test failed, with the report as the payload (printed once, on stderr).
pub fn run_sync_test<D, F, R>(meta: &TestMeta, body: F)
where
    D: DutModel,
    F: FnOnce(TestCtx, D) -> R,
    R: TestOutput,
{
    let env = RunEnv::from_process();
    let outcome = run_sync(meta, &env, body);
    report_test(meta, &env, outcome);
}

/// Replays for waves if asked, prints the report, and fails the calling `#[test]`.
fn report_test(meta: &TestMeta, env: &RunEnv, outcome: TestOutcome) {
    if let Some(error) = &outcome.run_dir_error {
        eprintln!("simjig: run directory not written: {error}");
    }
    let waves = env
        .waves
        .filter(|w| w.applies(outcome.failure.is_some()) && worth_replaying(w, &outcome))
        .map(|request| replay_with_waves(meta, env, &outcome, &request));
    if let Some(mut report) = outcome.failure {
        report.waves = waves;
        let text = report.to_string();
        if !meta.should_panic {
            eprintln!("{text}");
        }
        // `resume_unwind` skips the panic hook, so the block is printed once, and without
        // a misleading "panicked at <harness file>" line.
        panic::resume_unwind(Box::new(text));
    }
    if let Some(waves) = &waves {
        report_passing_replay(waves);
    }
}

/// Whether to replay a run `request` applies to. `SIMJIG_WAVES_ON_FAIL` is set for whole CI
/// runs and asks only for waves, so a failure none of whose models can trace is not
/// replayed (its report says why it has no waves). `SIMJIG_WAVES` asks for this run's
/// replay, which is also the determinism check, so it replays whatever the models.
fn worth_replaying(request: &WavesRequest, outcome: &TestOutcome) -> bool {
    match request.when {
        WavesWhen::Always => true,
        WavesWhen::OnFailure => outcome.failure.as_ref().is_some_and(|f| f.can_trace),
    }
}

/// After a passing run with `SIMJIG_WAVES`: prints where the waves are, and fails the test
/// if the replay did not reproduce the run.
fn report_passing_replay(waves: &WavesReplay) {
    eprintln!("simjig: waves of a passing run:\n{waves}");
    assert!(
        waves.reproduced(),
        "the replay with waves did not reproduce this passing run (see above)"
    );
}

/// Replays the run of `outcome` with waves (`harness::replay`), or says why it cannot.
fn replay_with_waves(
    meta: &TestMeta,
    env: &RunEnv,
    outcome: &TestOutcome,
    request: &WavesRequest,
) -> WavesReplay {
    let not_run = |why: &str| WavesReplay {
        files: Vec::new(),
        from: SimTime::ZERO,
        to: None,
        precision: outcome.precision.unwrap_or(Precision::PS),
        verdict: super::waves::Verdict::NotRun(why.to_owned()),
        events_compared: false,
    };
    let (Some(run_dir), Some(precision), Some(end)) =
        (&outcome.run_dir, outcome.precision, outcome.end_time)
    else {
        return not_run("this run has no run directory or no model to replay");
    };
    let Ok(exe) = std::env::current_exe() else {
        return not_run("the test binary's path is unknown");
    };
    let failure = outcome.failure.as_ref();
    let run = RunIdentity {
        test: outcome.test.clone(),
        package: meta.package.to_owned(),
        seed: outcome.seed,
        init: init_values(meta, env),
        ignored: meta.ignored,
        launcher: env.launcher.clone(),
        binary: env.binary.clone(),
    };
    let original = Original {
        run: &run,
        run_dir,
        end: RunEnd {
            failure: failure.map(|f| (f.kind.to_owned(), f.signature.to_string(), f.time.ticks())),
            end: Some(end.ticks()),
            trace_hash: outcome.trace_hash.map(|h| format!("{h:#018x}")),
        },
        target: failure.map_or(end, |f| f.time),
        precision,
        signals: failure.map(|f| f.signals.clone()).unwrap_or_default(),
        failed_branch: failure.and_then(|f| f.failed_branch),
    };
    replay::replay(&exe, &original, request)
}

/// Compile-time check that a test's declared output type is a [`TestOutput`]; the macro
/// spans the call at the user's return type, so the error points there.
pub const fn assert_test_output<T: TestOutput>() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(vars: &[(&str, &str)], cwd: &str) -> Option<String> {
        let var = |name: &str| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        };
        launcher(var, Some(Path::new(cwd)))
    }

    #[test]
    fn a_plain_shell_needs_no_launcher() {
        assert_eq!(detect(&[], "/w/crates/a"), None);
    }

    #[test]
    fn pixi_runs_through_pixi_run() {
        let pixi = [
            ("PIXI_PROJECT_ROOT", "/w"),
            ("PIXI_PROJECT_MANIFEST", "/w/pixi.toml"),
            ("PIXI_ENVIRONMENT_NAME", "default"),
            ("CONDA_PREFIX", "/w/.pixi/envs/default"),
        ];
        assert_eq!(detect(&pixi, "/w/crates/a").as_deref(), Some("pixi run"));
        assert_eq!(
            detect(&pixi, "/elsewhere").as_deref(),
            Some("pixi run --manifest-path /w/pixi.toml")
        );
        let mut conformance = pixi;
        conformance[2].1 = "conformance";
        assert_eq!(
            detect(&conformance, "/w").as_deref(),
            Some("pixi run -e conformance")
        );
    }

    #[test]
    fn a_conda_prefix_alone_is_recognized() {
        assert_eq!(
            detect(&[("CONDA_PREFIX", "/w/.pixi/envs/lint")], "/x").as_deref(),
            Some("pixi run --manifest-path /w -e lint")
        );
        assert_eq!(
            detect(&[("CONDA_PREFIX", "/opt/my envs/eda")], "/x").as_deref(),
            Some("conda run -p '/opt/my envs/eda'")
        );
    }
}
