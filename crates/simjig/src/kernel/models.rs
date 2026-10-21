//! [`Models`]: the backends of one simulation behind one flat slot space.
//!
//! A simulation drives one or more models (DUTs, or a DUT and a reference model) that share
//! time and the scheduler loop. The kernel numbers every model's slots in one *flat* range,
//! model 0's first, so the write log, the wait table and the clocks index a single table;
//! a [`RawSig`](super::signal::RawSig) carries `(model, slot)` and resolves to its flat
//! slot through the model's first flat slot ([`Models::starts`]).
//!
//! # Invariants
//! - Models are evaluated in declaration order, all of them in each delta, so a model's
//!   outputs written into another model's inputs by the testbench take effect in the next
//!   delta, never in an order that depends on scheduling.
//! - All models share one precision (one tick length); [`Models::new`] rejects a mix.
//! - The next model event is the minimum over the models whose `events_pending()` is
//!   true; `next_time_slot()` is never called on any other.

use std::fmt;
use std::panic::{self, AssertUnwindSafe};

use super::OrInvariant;
use super::backend::{EvalStatus, ModelBackend, ModelError};
use super::cell::{DirectTable, HeldEval};
use super::panics;
use super::tasks::panic_message;
use super::time::{Precision, SimTime};

/// Why a set of models cannot share one simulation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSetError {
    /// A simulation needs at least one model.
    Empty,
    /// Two models disagree on the length of a tick.
    PrecisionMismatch {
        /// Index of the model that disagrees with model 0.
        model: usize,
        /// Its precision.
        precision: Precision,
        /// Model 0's precision.
        expected: Precision,
    },
    /// The models have more than `u32::MAX` signals together.
    TooManySignals,
}

impl fmt::Display for ModelSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a simulation needs at least one model"),
            Self::PrecisionMismatch {
                model,
                precision,
                expected,
            } => write!(
                f,
                "model {model} has a precision of {precision} but model 0 has {expected}; \
                 the models of one simulation must share a timeprecision"
            ),
            Self::TooManySignals => f.write_str("the models have more than 2^32 signals"),
        }
    }
}

impl std::error::Error for ModelSetError {}

/// The failure of a model whose eval panicked: it is left mid-eval, so the run must stop as
/// it does for `$fatal`.
fn eval_panic(payload: &(dyn std::any::Any + Send)) -> ModelError {
    let at = panics::take().map_or_else(String::new, |l| format!(" at {l}"));
    ModelError::fatal(format!(
        "the model panicked during eval: {}{at}",
        panic_message(payload)
    ))
}

/// The models of one simulation; see the module docs.
pub(crate) struct Models {
    backends: Vec<Box<dyn ModelBackend>>,
    /// `first[m]` is model `m`'s first flat slot; the last entry is the total count.
    first: Vec<u32>,
    /// Per flat slot: `(model, the model's own slot)`.
    owner: Vec<(usize, u32)>,
    /// Per flat slot: its backend's [`Direct`] way to it, if any. Every entry came from a
    /// backend in `backends`, which lives exactly as long as this table.
    direct: DirectTable,
    /// Per model: evaluated, with nothing written since. An eval of such a model that
    /// [`settles`](ModelBackend::settles) would change nothing, so it is skipped.
    settled: Vec<bool>,
    /// Bumped whenever any model's state may have changed (a write, an eval that ran).
    version: u64,
    /// Bumped by every write.
    written: u64,
    /// Held for a synchronous run ([`sync_begin`](Self::sync_begin)).
    held: bool,
    /// While held: the held model's eval, called directly ([`ModelBackend::held_eval`]),
    /// and whether the model [`settles`](ModelBackend::settles), asked again after any
    /// eval that did not report `Running` (only that can change it while held).
    held_eval: Option<HeldEval>,
    held_settles: bool,
    /// Whether any model can have events of its own ([`ModelBackend::schedules_events`]).
    schedules: bool,
}

impl Models {
    /// The models, in declaration order.
    pub(crate) fn new(backends: Vec<Box<dyn ModelBackend>>) -> Result<Self, ModelSetError> {
        let expected = backends.first().ok_or(ModelSetError::Empty)?.precision();
        let mut first = vec![0u32];
        let mut owner = Vec::new();
        for (model, backend) in backends.iter().enumerate() {
            if backend.precision() != expected {
                return Err(ModelSetError::PrecisionMismatch {
                    model,
                    precision: backend.precision(),
                    expected,
                });
            }
            let count = backend.slot_count();
            let end = first[model]
                .checked_add(count)
                .ok_or(ModelSetError::TooManySignals)?;
            first.push(end);
            owner.extend((0..count).map(|slot| (model, slot)));
        }
        let settled = vec![false; backends.len()];
        let schedules = backends.iter().any(|b| b.schedules_events());
        let slots = owner
            .iter()
            .map(|&(model, slot)| backends[model].direct(slot))
            .collect();
        #[expect(unsafe_code, reason = "the table's contract, kept by this owner")]
        // SAFETY: the `Direct`s come from `backends`, which this `Models` owns for as long
        // as the table, and it reads and writes only between backend calls.
        let direct = unsafe { DirectTable::new(slots) };
        Ok(Self {
            backends,
            first,
            owner,
            direct,
            settled,
            version: 0,
            written: 0,
            held: false,
            held_eval: None,
            held_settles: false,
            schedules,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.backends.len()
    }

    /// Model `model`'s coverage counters (`ModelBackend::coverage`).
    pub(crate) fn coverage(&self, model: usize) -> Option<Vec<u32>> {
        self.backends.get(model)?.coverage()
    }

    /// Whether any model can write FST waves (`ModelBackend::can_trace`).
    pub(crate) fn can_trace(&self) -> bool {
        self.backends.iter().any(|b| b.can_trace())
    }

    /// Model `model`'s backend, for the coverage operations of `Sim`.
    pub(crate) fn backend_mut(&mut self, model: usize) -> Option<&mut dyn ModelBackend> {
        Some(&mut **self.backends.get_mut(model)?)
    }

    pub(crate) fn precision(&self) -> Precision {
        self.backends[0].precision()
    }

    /// Total number of flat slots.
    pub(crate) fn slot_count(&self) -> u32 {
        self.first[self.backends.len()]
    }

    /// The first flat slot of every model, plus the total count.
    pub(crate) fn starts(&self) -> &[u32] {
        &self.first
    }

    fn owner(&self, flat: u32) -> (&dyn ModelBackend, u32) {
        let (model, slot) = *self
            .owner
            .get(flat as usize)
            .or_invariant("invariant: flat slots come from Models::flat");
        (&*self.backends[model], slot)
    }

    pub(crate) fn slot_name(&self, flat: u32) -> &str {
        let (backend, slot) = self.owner(flat);
        backend.slot_name(slot)
    }

    pub(crate) fn slot_width(&self, flat: u32) -> u32 {
        let (backend, slot) = self.owner(flat);
        backend.slot_width(slot)
    }

    /// Reads flat slot `flat` into `out`. Inlined: a narrow slot is a load through its
    /// [`Direct`](super::cell::Direct); other slots take the backend's call.
    #[inline]
    pub(crate) fn read(&self, flat: u32, out: &mut [u32]) {
        if !self.direct.read(flat, out) {
            self.read_backend(flat, out);
        }
    }

    /// The value of flat slot `flat` if it has a direct way (a narrow slot of a backend
    /// that has one), as one word, low word first.
    #[inline]
    pub(crate) fn direct_u64(&self, flat: u32) -> Option<u64> {
        self.direct.read_u64(flat)
    }

    /// The value of flat slot `flat`, of `words` (one or two) words, as one word: low word
    /// first. Inlined, as [`read`](Self::read).
    #[inline]
    pub(crate) fn read_narrow(&self, flat: u32, words: usize) -> u64 {
        if let Some(value) = self.direct.read_u64(flat) {
            return value;
        }
        let mut buf = [0u32; 2];
        self.read_backend(flat, &mut buf[..words]);
        u64::from(buf[1]) << 32 | u64::from(buf[0])
    }

    #[inline(never)]
    fn read_backend(&self, flat: u32, out: &mut [u32]) {
        let (backend, slot) = self.owner(flat);
        backend.read(slot, out);
    }

    /// Writes `value` to flat slot `flat`. Inlined, as [`read`](Self::read).
    #[inline]
    pub(crate) fn write(&mut self, flat: u32, value: &[u32]) {
        let (model, slot) = self.owner[flat as usize];
        self.settled[model] = false;
        self.version += 1;
        self.written += 1;
        if !self.direct.write(flat, value) {
            self.write_backend(model, slot, value);
        }
    }

    #[inline(never)]
    fn write_backend(&mut self, model: usize, slot: u32, value: &[u32]) {
        self.backends[model].write(slot, value);
    }

    /// A copy of the slots' direct ways, for the kernel's signal handles.
    ///
    /// # Safety
    /// The copy is used only while this `Models` is alive, and only between backend calls.
    #[expect(
        unsafe_code,
        reason = "hands out a copy of the table under its contract"
    )]
    pub(crate) unsafe fn share_direct(&self) -> DirectTable {
        // SAFETY: the caller's promise is the table's contract.
        unsafe { self.direct.share() }
    }

    pub(crate) fn set_time(&mut self, time: SimTime) {
        for backend in &mut self.backends {
            backend.set_time(time);
        }
    }

    /// Evaluates every model in declaration order, stopping at the first that does not
    /// report `Running`; returns that model's index with its status.
    ///
    /// A panic inside a model's eval (a [`FakeModel`](super::fake::FakeModel) closure) is
    /// a fatal error of that model, naming the panic and its line: the model is left
    /// mid-eval, so the run must stop as it does for `$fatal`.
    ///
    /// A model that settles and was written nothing since its last eval is not evaluated:
    /// its backend promises that such an eval would be a no-op ([`ModelBackend::settles`]).
    /// That is a property of how the model was built, not of the writes alone: a Verilator
    /// model with a variable C++ may write (`public_flat_rw`, `--public-flat-rw`, a
    /// read-write probe) runs the logic that variable feeds on every eval, and so does not
    /// settle; nor does any Verilator model unless the test opted in
    /// (`ModelOptions::with_elide_evals`, which says when that is safe). The kernel still counts and records a skipped eval, so the trace (and its
    /// hash) is the same as if it ran. In a clocked testbench this skips the eval that
    /// opens each timestep and the last one of each delta loop.
    ///
    /// `time` is the current time (the last [`set_time`](Self::set_time)): a held model is
    /// evaluated with [`ModelBackend::eval_at`], which sets it again in the same call.
    pub(crate) fn eval(
        &mut self,
        time: SimTime,
    ) -> Result<(), (usize, Result<EvalStatus, ModelError>)> {
        if self.held {
            if self.settled[0] && self.held_settles {
                return Ok(());
            }
            self.version += 1;
            // No panic barrier: a held model runs no Rust code in its eval (`eval_at`).
            let status = match self.held_eval {
                Some(held) => {
                    // SAFETY: `held_eval` is set only while the model is held, by its
                    // backend, which this `Models` owns (`sync_begin`, `sync_end`).
                    #[expect(unsafe_code, reason = "the held eval's contract, kept as stated")]
                    let raw = unsafe { held.call(time.ticks()) };
                    if raw == 0 {
                        Ok(EvalStatus::Running)
                    } else {
                        self.backends[0].held_outcome(raw)
                    }
                }
                None => self.backends[0].eval_at(time),
            };
            if let Ok(EvalStatus::Running) = status {
                self.settled[0] = true;
                return Ok(());
            }
            self.held_settles = self.backends[0].settles();
            return Err((0, status));
        }
        for (model, backend) in self.backends.iter_mut().enumerate() {
            if self.settled[model] && backend.settles() {
                continue;
            }
            self.version += 1;
            panics::forget();
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| backend.eval()));
            let status = outcome.unwrap_or_else(|payload| Err(eval_panic(&*payload)));
            match status {
                Ok(EvalStatus::Running) => self.settled[model] = true,
                other => return Err((model, other)),
            }
        }
        Ok(())
    }

    /// A number that changes whenever any model's values may have changed: equal numbers
    /// mean nothing was written and nothing evaluated in between.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    /// A number that changes with every write (not with evals).
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// Starts a run ([`ModelBackend::sync_begin`]); only a lone model is held, since a held
    /// model must be the only one evaluated.
    pub(crate) fn sync_begin(&mut self) {
        if let [backend] = self.backends.as_mut_slice() {
            self.held = backend.sync_begin();
            if self.held {
                self.held_eval = backend.held_eval();
                self.held_settles = backend.settles();
            }
        }
    }

    /// Ends [`sync_begin`](Self::sync_begin).
    pub(crate) fn sync_end(&mut self) {
        self.held = false;
        self.held_eval = None;
        for backend in &mut self.backends {
            backend.sync_end();
        }
    }

    /// The held model's eval, while held ([`ModelBackend::held_eval`]).
    pub(crate) fn held_eval(&self) -> Option<HeldEval> {
        if self.held {
            self.backends[0].held_eval()
        } else {
            None
        }
    }

    /// What the held model's non-OK raw status means ([`ModelBackend::held_outcome`]).
    pub(crate) fn held_outcome(
        &mut self,
        raw: i32,
    ) -> Result<(), (usize, Result<EvalStatus, ModelError>)> {
        match self.backends[0].held_outcome(raw) {
            Ok(EvalStatus::Running) => Ok(()),
            other => Err((0, other)),
        }
    }

    /// Whether the models are held for a synchronous run: no backend call can run while
    /// the testbench does (no waves to sample, no DPI-C import implemented in Rust).
    pub(crate) fn held(&self) -> bool {
        self.held
    }

    /// Evaluates every model at `time`, in declaration order, as [`eval`](Self::eval) does
    /// (with the same handling of a panic), through [`ModelBackend::eval_at`]; for a
    /// synchronous run, where every eval runs.
    pub(crate) fn eval_at(
        &mut self,
        time: SimTime,
    ) -> Result<(), (usize, Result<EvalStatus, ModelError>)> {
        self.version += 1;
        if self.held {
            // A held model runs no Rust code in its eval (no DPI-C import implemented in
            // Rust, no waves), so nothing can panic there to catch.
            return match self.backends[0].eval_at(time) {
                Ok(EvalStatus::Running) => Ok(()),
                other => Err((0, other)),
            };
        }
        for (model, backend) in self.backends.iter_mut().enumerate() {
            panics::forget();
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| backend.eval_at(time)));
            match outcome.unwrap_or_else(|payload| Err(eval_panic(&*payload))) {
                Ok(EvalStatus::Running) => {}
                other => return Err((model, other)),
            }
        }
        Ok(())
    }

    /// The earliest pending model event and the first model that has it, asking only
    /// models with `events_pending()` for their `next_time_slot()`.
    pub(crate) fn next_event(&self) -> Option<(usize, SimTime)> {
        if !self.schedules {
            return None;
        }
        let mut next: Option<(usize, SimTime)> = None;
        for (model, backend) in self.backends.iter().enumerate() {
            if !backend.events_pending() {
                continue;
            }
            let t = backend.next_time_slot();
            if next.is_none_or(|(_, n)| t < n) {
                next = Some((model, t));
            }
        }
        next
    }
}
