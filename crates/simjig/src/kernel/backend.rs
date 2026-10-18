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

use std::fmt;

use super::time::{Precision, SimTime};

/// What an `eval` reported, when it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EvalStatus {
    /// The model can keep running.
    Running,
    /// The design executed `$finish`.
    Finished,
}

/// A fatal error raised by the model during `eval` (`$fatal`, `$stop`, an internal error).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelError {
    /// What the model reported, including the HDL file, line and scope when known.
    pub(crate) message: String,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
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
    /// Evaluates the model until it is stable at the current time.
    fn eval(&mut self) -> Result<EvalStatus, ModelError>;
    /// Whether the model has scheduled events of its own (`--timing` delays).
    fn events_pending(&self) -> bool;
    /// The time of the model's next scheduled event; only valid if `events_pending()`.
    fn next_time_slot(&self) -> SimTime;
}
