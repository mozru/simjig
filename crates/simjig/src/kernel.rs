//! The kernel: a single-threaded, deterministic async scheduler that owns simulation time
//! and drives a model through the narrow [`ModelBackend`](backend::ModelBackend) interface.
//!
//! Map of the concepts, one module each:
//! - `sched`: the normative per-timestamp loop (start here);
//! - `tasks`: spawn, task handles, structured cancellation by drop, panic capture;
//! - `waits` and `trigger`: trigger registrations and the trigger futures;
//! - `signal`: `RawSig` handles and the write log;
//! - `clock`: kernel-owned clocks; `time`: ticks and checked durations;
//! - `sync`: `Event`, `Lock`, `Queue`; `combinator`: `join!`, `first!`, `with_timeout`,
//!   whose operands run as helper tasks;
//! - `failure`, `recorder`: failures as data and the flight recorder; `panics`: where a
//!   task (or, through `panics::take`, a DPI-C import) panicked;
//! - `state`: the kernel struct and the thread-local current kernel;
//! - `waker`: the only `unsafe` in the kernel;
//! - `backend`, `fake`: the model interface and its pure-Rust implementation; `models`:
//!   the simulation's models behind one flat slot space;
//! - `differential` (tests only): random programs whose traces are pinned to a corpus.
//!
//! The kernel imports nothing from `crate::model`, so all of it is tested against
//! [`FakeModel`](fake::FakeModel).

pub(crate) mod backend;
pub(crate) mod cell;
pub(crate) mod clock;
pub(crate) mod combinator;
#[cfg(test)]
mod differential;
pub(crate) mod failure;
pub(crate) mod fake;
pub(crate) mod models;
pub(crate) mod panics;
pub(crate) mod recorder;
pub(crate) mod sched;
pub(crate) mod signal;
pub(crate) mod sim;
mod slab;
pub(crate) mod state;
pub(crate) mod sync;
pub(crate) mod sync_clock;
pub(crate) mod tasks;
pub(crate) mod time;
pub(crate) mod trigger;
mod waits;
mod waker;

/// Unwraps a value whose absence would be a kernel bug, panicking with the violated
/// invariant (the workspace denies `expect`, which hides this intent).
pub(crate) trait OrInvariant<T> {
    /// The value, or a panic naming `invariant`.
    fn or_invariant(self, invariant: &str) -> T;
}

impl<T> OrInvariant<T> for Option<T> {
    #[track_caller]
    fn or_invariant(self, invariant: &str) -> T {
        match self {
            Some(v) => v,
            None => panic!("{invariant}"),
        }
    }
}

impl<T, E: std::fmt::Debug> OrInvariant<T> for Result<T, E> {
    #[track_caller]
    fn or_invariant(self, invariant: &str) -> T {
        match self {
            Ok(v) => v,
            Err(e) => panic!("{invariant}: {e:?}"),
        }
    }
}
