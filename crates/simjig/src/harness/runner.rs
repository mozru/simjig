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

use crate::{Failure, Sim, SimDuration, SimTime, TimeUnit};

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
    /// The command prefix the repro line needs (`pixi run`), detected by `RunEnv::from_process`.
    pub launcher: Option<String>,
    /// The test binary's name as nextest's `binary()` filter matches it
    /// (`harness_tests`), from the executable's name.
    pub binary: Option<String>,
}

impl RunEnv {
    /// Reads `SIMJIG_SEED`, `SIMJIG_RUNS_DIR`, and the pixi/conda variables that decide
    /// the repro's launcher (`PIXI_WORKSPACE_ROOT`, `PIXI_PROJECT_ROOT`, `CONDA_PREFIX`,
    /// ...; see `runner::launcher`).
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
        let cwd = std::env::current_dir().ok();
        let launcher = launcher(|name| std::env::var(name).ok(), cwd.as_deref());
        let binary = std::env::current_exe()
            .ok()
            .and_then(|exe| rundir::binary_name(&exe));
        Self {
            seed,
            runs_root,
            launcher,
            binary,
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
        launcher: env.launcher.clone(),
        binary: env.binary.clone(),
    };
    let executed = execute(meta, &options, ctx, body);
    finish(run, env, &options, executed)
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
        launcher: run.launcher.as_deref(),
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
