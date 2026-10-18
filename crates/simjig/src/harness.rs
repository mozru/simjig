//! The test harness: what makes a simjig test a normal Rust test.
//!
//! `#[simjig::test]` (in `simjig-macros`) turns an `async fn` into a `#[test]` that calls
//! `runner::run_test`. For each run the harness:
//! 1. picks the root [`Seed`](crate::Seed) (`SIMJIG_SEED`, or a hash of the test name) and derives the
//!    model's options from it ([`ModelOptions`](crate::ModelOptions), including Verilator's own seed);
//! 2. builds the simulation and the DUT through the [`DutModel`](crate::DutModel) seam;
//! 3. applies the test's simulation-time `timeout` as the kernel's time limit;
//! 4. runs the body as the main task, with a [`TestCtx`](crate::TestCtx) for named random streams;
//! 5. renders any [`Failure`](crate::Failure) as one [`FailureReport`](crate::FailureReport) block with a
//!    triage signature and a one-line repro, and writes the run directory
//!    (`<target>/simjig/runs/<package>/<binary>/<test>/<seed>/`, see `rundir`).
//!
//! # Environment
//! - `SIMJIG_SEED`: the root seed (`0x`-hex or decimal), as printed by a failure report.
//! - `SIMJIG_RUNS_DIR`: where run directories go, instead of `<target>/simjig/runs`.
//! - `PIXI_WORKSPACE_ROOT`, `PIXI_PROJECT_ROOT`, `CONDA_PREFIX`: when set, the repro line
//!   runs `cargo` through `pixi run` (or `conda run`), as `RunEnv::from_process` says.

pub(crate) mod ctx;
pub(crate) mod dut;
pub(crate) mod report;
pub(crate) mod rundir;
pub(crate) mod runner;
pub(crate) mod seed;

/// What a `#[simjig::test]` body returns when it can fail: `Ok(())` or the
/// [`Failure`](crate::Failure) that ends the test.
pub type Result<T = (), E = crate::Failure> = core::result::Result<T, E>;

/// Includes the DUT bindings simjig-build generated for the design `name`, as the module
/// `name`.
///
/// ```ignore
/// simjig::include_dut!(fifo);          // mod fifo { ... }, with fifo::Dut
///
/// #[simjig::test]
/// async fn smoke(dut: fifo::Dut) { /* ... */ }
/// ```
///
/// # Contract with simjig-build
/// The build script of the crate that calls `include_dut!(name)` runs simjig-build, which
/// writes `$OUT_DIR/simjig/<name>.rs`. That file:
/// - holds only items (no inner attributes or `//!` docs: it is spliced into
///   `pub mod <name> { ... }`);
/// - defines `pub struct Dut` implementing [`DutModel`](crate::DutModel), plus whatever
///   types its signals need;
/// - refers to simjig only through absolute `::simjig::...` paths.
///
/// The `@from <dir>` form reads `<dir>/<name>.rs` instead; simjig's own tests use it
/// with hand-written files.
#[macro_export]
macro_rules! include_dut {
    ($name:ident) => {
        $crate::include_dut!(@from ::core::concat!(::core::env!("OUT_DIR"), "/simjig"), $name);
    };
    (@from $dir:expr, $name:ident) => {
        #[doc = ::core::concat!("DUT bindings for the design `", ::core::stringify!($name), "`.")]
        pub mod $name {
            ::core::include!(::core::concat!($dir, "/", ::core::stringify!($name), ".rs"));
        }
    };
}
