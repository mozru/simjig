//! [`ModelBackend`]: the narrow interface the kernel needs from a simulated model.
//!
//! The kernel never touches model memory directly. It sees a model as a table of *slots*
//! (signals), numbered `0..slot_count()`, each with a name and a bit width, whose values it
//! reads and writes as little-endian `u32` words (word 0 holds bits 31..0). Everything else
//! (time, evaluation, `--timing` events) goes through the methods below.
//!
//! Two implementations exist: [`FakeModel`](super::fake::FakeModel), Rust closures used to
//! test the kernel without Verilator, and the Verilator backend in `model/`.
//!
//! # Contract
//! - `read` returns the value as of the last `eval` (or the last `write`, which the kernel
//!   only issues just before an `eval`).
//! - `write` receives exactly `words(width)` words with the bits above `width` clear; the
//!   kernel enforces both before calling it (Verilator assumes clean upper bits).
//! - `next_time_slot` is called only when `events_pending` returned `true` (Verilator's
//!   `nextTimeSlot()` aborts otherwise).
//! - No method calls back into the kernel.
//! - A simulation may drive several backends on one thread, calling them in turn. Each
//!   backend must make itself the current model before it runs model code: a Verilator
//!   backend sets `Verilated::threadContextp()` to its own context (and swaps in its own
//!   `$urandom` state) in `eval`, and in any other call that runs generated code, because
//!   `$time`, `$finish`, `$fatal` and `$urandom` go through the thread's context, not the
//!   model's (docs/notes/spikes.md §2). The kernel never switches contexts itself.

use std::fmt;
use std::path::Path;

use super::cell::Direct;
use super::failure::SourceLocation;
use super::time::{Precision, SimTime};

/// What an `eval` reported, when it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EvalStatus {
    /// The model can keep running.
    Running,
    /// The design executed `$finish`.
    Finished,
}

/// Which kind of failure a [`ModelError`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelErrorKind {
    /// The design or Verilator reported a fatal error (`$fatal`, `$stop`, an internal
    /// error).
    Fatal,
    /// A DPI-C import implemented in Rust panicked during the eval.
    DpiPanic,
    /// The test's setup cannot run (a DPI-C import without an implementation, ...); the
    /// design is not at fault. What to change.
    Setup {
        /// The advice for the failure's `help:` line.
        help: &'static str,
    },
}

/// A fatal error raised by the model during `eval`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelError {
    /// What failed; it decides the run's [`FailureKind`](super::failure::FailureKind).
    pub(crate) kind: ModelErrorKind,
    /// What the model reported, including the HDL file, line and scope when known.
    pub(crate) message: String,
    /// The Rust line that panicked, for a [`ModelErrorKind::DpiPanic`].
    pub(crate) location: Option<&'static SourceLocation>,
}

impl ModelError {
    /// A fatal error reported by the design or Verilator.
    pub(crate) const fn fatal(message: String) -> Self {
        Self {
            kind: ModelErrorKind::Fatal,
            message,
            location: None,
        }
    }

    /// A setup problem the model found while evaluating; `help` says what to change.
    pub(crate) const fn setup(message: String, help: &'static str) -> Self {
        Self {
            kind: ModelErrorKind::Setup { help },
            message,
            location: None,
        }
    }

    /// A panic in a DPI-C import implemented in Rust, at `location` if the panic hook saw
    /// it.
    pub(crate) const fn dpi_panic(
        message: String,
        location: Option<&'static SourceLocation>,
    ) -> Self {
        Self {
            kind: ModelErrorKind::DpiPanic,
            message,
            location,
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Why a coverage operation on a backend did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CoverageFault {
    /// The model has no coverage counters (not built with coverage, or not Verilator).
    NotBuilt,
    /// The model refused or failed; why.
    Failed(String),
    /// The file could not be written; why.
    Write(String),
}

/// Number of `u32` words that hold a value of `width` bits.
#[must_use]
pub(crate) const fn words(width: u32) -> usize {
    width.div_ceil(32) as usize
}

/// The operations the kernel performs on a model. See the module docs for the contract.
pub(crate) trait ModelBackend {
    /// The length of one tick (the design's `timeprecision`).
    fn precision(&self) -> Precision;
    /// Number of slots; slots are `0..slot_count()`.
    fn slot_count(&self) -> u32;
    /// The hierarchical name of `slot`, used in every diagnostic.
    fn slot_name(&self, slot: u32) -> &str;
    /// The width of `slot` in bits (at least 1).
    fn slot_width(&self, slot: u32) -> u32;
    /// Copies the value of `slot` into `out`, which holds exactly `words(width)` words.
    fn read(&self, slot: u32, out: &mut [u32]);
    /// Deposits `value` into `slot`; it takes effect for the next `eval`.
    fn write(&mut self, slot: u32, value: &[u32]);
    /// Sets the model's notion of current time; called before every `eval` at a new time.
    fn set_time(&mut self, time: SimTime);
    /// Evaluates the model until it is stable at the current time. Switches the thread's
    /// Verilator context to this model first (see the module docs).
    fn eval(&mut self) -> Result<EvalStatus, ModelError>;
    /// A direct way to the narrow storage of `slot`, if the backend has one: the kernel
    /// then reads and writes the slot through it instead of [`read`](Self::read) and
    /// [`write`](Self::write), which must behave the same. `None` (the default) for a slot
    /// or backend without one.
    fn direct(&self, _slot: u32) -> Option<Direct> {
        None
    }
    /// Starts a synchronous run (`Sim::run_sync`): until [`sync_end`](Self::sync_end), every
    /// eval comes through [`eval_at`](Self::eval_at) and no other model of the simulation is
    /// evaluated, so the backend may keep its model installed between evals.
    fn sync_begin(&mut self) {}
    /// Ends [`sync_begin`](Self::sync_begin).
    fn sync_end(&mut self) {}
    /// [`set_time`](Self::set_time) then [`eval`](Self::eval), which a backend may do in
    /// one call.
    fn eval_at(&mut self, time: SimTime) -> Result<EvalStatus, ModelError> {
        self.set_time(time);
        self.eval()
    }
    /// Whether an `eval` may change `slot`'s value; `false` only for a slot that nothing
    /// but [`write`](Self::write) changes. `true` (the default) is always safe.
    fn eval_changes(&self, _slot: u32) -> bool {
        true
    }
    /// Whether an `eval` with no `write` since the previous `eval` is sure to change
    /// nothing and call nothing (no timed events of its own, no per-eval waves sampling):
    /// the eval loop has already settled every value. [`Models`](super::models::Models)
    /// then skips such an eval; `false` (the default) evaluates every time.
    fn settles(&self) -> bool {
        false
    }
    /// Whether the model has scheduled events of its own (`--timing` delays).
    fn events_pending(&self) -> bool;
    /// The time of the model's next scheduled event; only valid if `events_pending()`.
    fn next_time_slot(&self) -> SimTime;
    /// The model's coverage counters, if it has any (a Verilator model built with
    /// `--coverage`); `None` by default.
    fn coverage(&self) -> Option<Vec<u32>> {
        None
    }
    /// Whether the model can write FST waves (a Verilator model built with `--trace-fst`);
    /// `false` by default.
    fn can_trace(&self) -> bool {
        false
    }
    /// Sets every coverage counter to zero.
    fn zero_coverage(&mut self) -> Result<(), CoverageFault> {
        Err(CoverageFault::NotBuilt)
    }
    /// Writes Verilator's coverage file (`coverage.dat`: every point's name and count) to
    /// `path`.
    fn write_coverage(&mut self, _path: &Path) -> Result<(), CoverageFault> {
        Err(CoverageFault::NotBuilt)
    }
}
