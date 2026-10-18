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

use super::OrInvariant;
use super::backend::{EvalStatus, ModelBackend, ModelError};
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

/// The models of one simulation; see the module docs.
pub(crate) struct Models {
    backends: Vec<Box<dyn ModelBackend>>,
    /// `first[m]` is model `m`'s first flat slot; the last entry is the total count.
    first: Vec<u32>,
    /// Per flat slot: `(model, the model's own slot)`.
    owner: Vec<(usize, u32)>,
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
        Ok(Self {
            backends,
            first,
            owner,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.backends.len()
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

    pub(crate) fn read(&self, flat: u32, out: &mut [u32]) {
        let (backend, slot) = self.owner(flat);
        backend.read(slot, out);
    }

    pub(crate) fn write(&mut self, flat: u32, value: &[u32]) {
        let (model, slot) = self.owner[flat as usize];
        self.backends[model].write(slot, value);
    }

    pub(crate) fn set_time(&mut self, time: SimTime) {
        for backend in &mut self.backends {
            backend.set_time(time);
        }
    }

    /// Evaluates every model in declaration order, stopping at the first that does not
    /// report `Running`; returns that model's index with its status.
    pub(crate) fn eval(&mut self) -> Result<(), (usize, Result<EvalStatus, ModelError>)> {
        for (model, backend) in self.backends.iter_mut().enumerate() {
            match backend.eval() {
                Ok(EvalStatus::Running) => {}
                other => return Err((model, other)),
            }
        }
        Ok(())
    }

    /// The earliest pending model event and the first model that has it, asking only
    /// models with `events_pending()` for their `next_time_slot()`.
    pub(crate) fn next_event(&self) -> Option<(usize, SimTime)> {
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
