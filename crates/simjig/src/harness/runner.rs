//! Running one `#[simjig::test]`: seed, model, timeout, body, report, run directory.
//!
//! [`run`] does the work and returns a [`TestOutcome`]; [`run_test`], which the macro calls,
//! reads the environment, runs, and turns a failure into a test failure. Keeping [`run`]
//! free of process state (environment, panics) lets the harness's own tests drive it.
//!
//! The kernel records a `TaskPanic` without its source location (the panic hook prints it,
//! the payload does not carry it). The harness wraps the panic hook to remember the
//! location of each panic on the test's thread during a run, and attaches the one whose
//! message matches the failure.

use std::cell::RefCell;
use std::future::Future;
use std::panic;
use std::path::PathBuf;
use std::sync::Once;

use crate::{Failure, FailureKind, Sim, SimDuration, SimTime, TimeUnit};

use super::ctx::TestCtx;
use super::dut::{DutModel, ModelOptions};
use super::report::{FailureReport, RunIdentity};
use super::rundir::{self, Json, Summary};
use super::seed::{Seed, SeedSource, parse_seed};

/// A test's simulation-time budget, as written in `#[simjig::test(timeout = "2ms")]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeout {
    /// The amount.
    pub value: u64,
    /// Its unit.
    pub unit: TimeUnit,
}

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
    pub timeout: Option<Timeout>,
    /// Whether the test has an `#[ignore]` attribute.
    pub ignored: bool,
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
    /// Where run directories go (`SIMJIG_RUNS_DIR`, else `<target>/simjig/runs`); `None`
    /// writes none.
    pub runs_root: Option<PathBuf>,
}

impl RunEnv {
    /// Reads `SIMJIG_SEED` and `SIMJIG_RUNS_DIR`.
    ///
    /// # Panics
    /// If `SIMJIG_SEED` is set but is not a seed (the message says what is accepted).
    #[must_use]
    pub fn from_process() -> Self {
        let seed = std::env::var("SIMJIG_SEED")
            .ok()
            .map(|text| parse_seed(&text).unwrap_or_else(|e| panic!("{e}")));
        let runs_root = std::env::var_os("SIMJIG_RUNS_DIR")
            .map(PathBuf::from)
            .or_else(rundir::default_root);
        Self { seed, runs_root }
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
    timeout: Option<SimDuration>,
    result: Result<(), Failure>,
}

/// Runs a test: builds `D`, runs `body` as the main task under the test's timeout, and
/// writes the run directory.
///
/// # Panics
/// If the test's `timeout` is not a whole number of ticks of the design's precision.
pub fn run<D, F, Fut>(meta: &TestMeta, env: &RunEnv, body: F) -> TestOutcome
where
    D: DutModel,
    F: FnOnce(TestCtx, D) -> Fut,
    Fut: Future + 'static,
    Fut::Output: TestOutput,
{
    let test = meta.test_path();
    let seed = env.seed.map_or_else(
        || Seed::from_test_name(meta.package, &test),
        |value| Seed::new(value, SeedSource::Env),
    );
    let options = ModelOptions::new(seed);
    let ctx = TestCtx::new(test.clone(), seed, options.verilator_seed);
    let run = RunIdentity {
        test,
        package: meta.package.to_owned(),
        seed,
        ignored: meta.ignored,
    };
    start_capture();
    let executed = execute(meta, &options, ctx, body);
    let panics = finish_capture();
    finish(run, env, &options, executed, &panics)
}

fn execute<D, F, Fut>(meta: &TestMeta, options: &ModelOptions, ctx: TestCtx, body: F) -> Executed
where
    D: DutModel,
    F: FnOnce(TestCtx, D) -> Fut,
    Fut: Future + 'static,
    Fut::Output: TestOutput,
{
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
    let timeout = meta.timeout.map(|t| {
        sim.precision()
            .duration(t.value, t.unit)
            .unwrap_or_else(|e| {
                panic!(
                    "#[simjig::test(timeout = ...)] of `{}`: {e}",
                    ctx.test_name()
                )
            })
    });
    if let Some(timeout) = timeout {
        sim.set_time_limit(SimTime(timeout.ticks()));
    }
    let result = sim.run(body(ctx, dut)).and_then(TestOutput::into_result);
    Executed {
        sim: Some(sim),
        timeout,
        result,
    }
}

fn finish(
    run: RunIdentity,
    env: &RunEnv,
    options: &ModelOptions,
    executed: Executed,
    panics: &[(String, String)],
) -> TestOutcome {
    let Executed {
        sim,
        timeout,
        result,
    } = executed;
    let dir = env
        .runs_root
        .as_ref()
        .map(|root| rundir::run_dir(root, &run.test, run.seed));
    let mut failure = result.err().map(|failure| {
        let location = match &failure.kind {
            FailureKind::TaskPanic { message } => panic_location(panics, message),
            _ => None,
        };
        let mut report = FailureReport::new(&failure, run.clone(), location, timeout);
        report.run_dir.clone_from(&dir);
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
        trace_hash: sim.as_ref().map(Sim::trace_hash),
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
    timeout: Option<SimDuration>,
    failure: Option<&'a FailureReport>,
) -> Summary<'a> {
    let precision = sim.map(Sim::precision);
    Summary {
        test: &run.test,
        package: &run.package,
        seed: run.seed,
        verilator_seed: options.verilator_seed,
        precision: precision.map(|p| p.to_string()),
        end_time: sim.map_or(Json::Null, |s| {
            Json::time(s.now().0, s.now().display(s.precision()))
        }),
        timeout: match (timeout, precision) {
            (Some(t), Some(p)) => Json::time(t.ticks(), SimTime(t.ticks()).display(p)),
            _ => Json::Null,
        },
        trace_hash: sim.map(Sim::trace_hash),
        events: None,
        failure,
    }
}

fn write_run_dir(
    dir: &std::path::Path,
    mut summary: Summary<'_>,
    sim: Option<&Sim>,
) -> std::io::Result<()> {
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
    let outcome = run(meta, &RunEnv::from_process(), body);
    if let Some(error) = &outcome.run_dir_error {
        eprintln!("simjig: run directory not written: {error}");
    }
    if let Some(report) = outcome.failure {
        let text = report.to_string();
        eprintln!("{text}");
        // `resume_unwind` skips the panic hook, so the block is printed once, and without
        // a misleading "panicked at <harness file>" line.
        panic::resume_unwind(Box::new(text));
    }
}

/// Compile-time check that a test's declared output type is a [`TestOutput`]; the macro
/// spans the call at the user's return type, so the error points there.
pub const fn assert_test_output<T: TestOutput>() {}

/// Panics seen on this thread during the current run: `(message, file:line:col)`.
const PANICS_KEPT: usize = 64;

thread_local! {
    static PANICS: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
}

static HOOK: Once = Once::new();

/// Starts recording panic locations on this thread (installing the hook once per process;
/// it chains to the previous hook, so panics still print as usual).
fn start_capture() {
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let message = info
                .payload_as_str()
                .unwrap_or("<non-string panic payload>")
                .to_owned();
            let location = info.location().map(ToString::to_string);
            PANICS.with_borrow_mut(|panics| {
                if let (Some(panics), Some(location)) = (panics, location) {
                    if panics.len() == PANICS_KEPT {
                        panics.remove(0);
                    }
                    panics.push((message, location));
                }
            });
            previous(info);
        }));
    });
    PANICS.set(Some(Vec::new()));
}

fn finish_capture() -> Vec<(String, String)> {
    PANICS.take().unwrap_or_default()
}

/// The location of the most recent panic with `message`.
fn panic_location(panics: &[(String, String)], message: &str) -> Option<String> {
    panics
        .iter()
        .rev()
        .find(|(m, _)| m == message)
        .map(|(_, location)| location.clone())
}
