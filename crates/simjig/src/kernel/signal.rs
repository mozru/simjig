//! [`RawSig`] handles and the write log.
//!
//! A `RawSig` is a `Copy` id (simulation, model, slot) resolved through the current kernel
//! on every access; no Rust reference to model memory ever exists. A handle used outside
//! its own running simulation panics, naming the signal. The model index says which of
//! the simulation's models (in declaration order) the slot belongs to.
//!
//! **Reads** return the model's current value: a write is never visible before it is
//! applied. **Writes** are deposits (VPI inertial puts, as cocotb 2.1 uses on Verilator):
//! they go into the write log and are applied, in write order, right after the next eval
//! (see the scheduler loop in `sched`). Within one delta (between two applications):
//! - the last write to a signal wins;
//! - two different writers (tasks or a kernel clock) writing *different* values to one
//!   signal is a [`WriteConflict`](super::failure::FailureKind::WriteConflict) naming both,
//!   because the outcome would depend on scheduling order. cocotb has no such check (last
//!   write wins); writers that agree on the value are accepted, since order cannot matter;
//! - any write in the `ReadOnly` phase is a
//!   [`WriteInReadOnly`](super::failure::FailureKind::WriteInReadOnly) failure.

use std::fmt;
use std::marker::PhantomData;
use std::panic::Location;

use super::OrInvariant;
use super::backend::words;
use super::failure::FailureKind;
use super::slab::Key;
use super::state::{self, Kernel, Phase};

/// A handle to one model signal, valid inside the [`Sim::run`](crate::Sim::run) of the
/// simulation it came from. `Copy`, and deliberately `!Send`: the kernel is single-threaded.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawSig {
    sim_gen: u32,
    model: u32,
    slot: u32,
    _single_thread: PhantomData<*const ()>,
}

impl fmt::Debug for RawSig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = state::retired_name(self.sim_gen, self.model(), self.slot);
        f.debug_struct("RawSig")
            .field("sim", &self.sim_gen)
            .field("model", &self.model)
            .field("slot", &self.slot)
            .field("name", &name)
            .finish()
    }
}

impl RawSig {
    /// The handle of slot `slot` of model `model` (declaration order).
    pub(crate) const fn in_model(sim_gen: u32, model: u32, slot: u32) -> Self {
        Self {
            sim_gen,
            model,
            slot,
            _single_thread: PhantomData,
        }
    }

    pub(crate) const fn sim_gen(self) -> u32 {
        self.sim_gen
    }

    /// The slot within its own model (not the kernel's flat slot).
    pub(crate) const fn local_slot(self) -> u32 {
        self.slot
    }

    /// Which of its simulation's models the signal belongs to (0 for the first).
    #[must_use]
    pub const fn model(self) -> usize {
        self.model as usize
    }

    fn label(self) -> String {
        state::retired_name(self.sim_gen, self.model(), self.slot).map_or_else(
            || {
                format!(
                    "slot {} of model {} of simulation #{}",
                    self.slot, self.model, self.sim_gen
                )
            },
            |n| format!("`{n}`"),
        )
    }

    /// Resolves the handle on the current kernel, passing its flat slot to `f`.
    ///
    /// # Panics
    /// Outside a running simulation, or in a different simulation than the handle's.
    #[track_caller]
    pub(crate) fn with<R>(self, f: impl FnOnce(&Kernel, u32) -> R) -> R {
        let resolved = state::try_current(|k| {
            let flat = state::flat_slot(&k.starts, self.model(), self.slot);
            match flat {
                Some(flat) if k.sim_gen == self.sim_gen => Some(f(k, flat)),
                _ => None,
            }
        });
        match resolved {
            Some(Some(r)) => r,
            Some(None) => panic!(
                "signal {} belongs to simulation #{}, not the one running now; handles are \
                 not valid across simulations",
                self.label(),
                self.sim_gen
            ),
            None => panic!(
                "signal {} used outside a running simulation; signal handles work only \
                 inside Sim::run",
                self.label()
            ),
        }
    }

    /// The signal's hierarchical name.
    ///
    /// # Panics
    /// Outside its own running simulation.
    #[track_caller]
    #[must_use]
    pub fn name(self) -> String {
        self.with(|k, slot| k.names[slot as usize].to_string())
    }

    /// The signal's width in bits.
    ///
    /// # Panics
    /// Outside its own running simulation.
    #[track_caller]
    #[must_use]
    pub fn width(self) -> u32 {
        self.with(|k, slot| k.widths[slot as usize])
    }

    /// Copies the current value into `out`, as little-endian `u32` words.
    ///
    /// # Panics
    /// Outside its own running simulation, or if `out` is not `width.div_ceil(32)` words.
    #[track_caller]
    pub fn read_words(self, out: &mut [u32]) {
        self.with(|k, slot| {
            let n = words(k.widths[slot as usize]);
            assert_eq!(
                out.len(),
                n,
                "signal `{}` needs a {n}-word buffer",
                k.names[slot as usize]
            );
            k.models.borrow().read(slot, out);
        });
    }

    /// The current value, for signals of at most 64 bits.
    ///
    /// # Panics
    /// Outside its own running simulation, or if the signal is wider than 64 bits.
    #[track_caller]
    #[must_use]
    pub fn read_u64(self) -> u64 {
        self.with(|k, slot| {
            let width = k.widths[slot as usize];
            assert!(
                width <= 64,
                "signal `{}` is {width} bits wide; use read_words",
                k.names[slot as usize]
            );
            let mut buf = [0u32; 2];
            let buf = &mut buf[..words(width)];
            k.models.borrow().read(slot, buf);
            buf.iter()
                .rev()
                .fold(0u64, |acc, &w| acc << 32 | u64::from(w))
        })
    }

    /// Logs a write of `value` (little-endian `u32` words), applied after the next eval.
    ///
    /// # Panics
    /// Outside its own running simulation, if `value` has the wrong number of words, or if
    /// bits above the signal's width are set.
    #[track_caller]
    pub fn write_words(self, value: &[u32]) {
        let location = Location::caller();
        self.with(|k, slot| {
            let width = k.widths[slot as usize];
            let name = &k.names[slot as usize];
            let n = words(width);
            assert_eq!(value.len(), n, "signal `{name}` takes {n} words");
            let spare = n * 32 - width as usize;
            assert!(
                spare == 0 || value[n - 1] >> (32 - spare) == 0,
                "value does not fit signal `{name}` ({width} bits)"
            );
            write(k, slot, value, Writer::current(k), Some(location));
        });
    }

    /// Logs a write of `value`, applied after the next eval.
    ///
    /// # Panics
    /// Outside its own running simulation, or if `value` does not fit the signal's width.
    #[track_caller]
    pub fn write_u64(self, value: u64) {
        let location = Location::caller();
        self.with(|k, slot| {
            let width = k.widths[slot as usize];
            let name = &k.names[slot as usize];
            assert!(
                width >= 64 || value >> width == 0,
                "value {value:#x} does not fit signal `{name}` ({width} bits)"
            );
            let low = u32::try_from(value & 0xffff_ffff).or_invariant("invariant: masked");
            let high = u32::try_from(value >> 32).or_invariant("invariant: shifted");
            let n = words(width);
            let mut buf = vec![0u32; n];
            buf[0] = low;
            if n > 1 {
                buf[1] = high;
            }
            write(k, slot, &buf, Writer::current(k), Some(location));
        });
    }
}

/// Who wrote a log entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Writer {
    Task(Key),
    Clock,
    /// Kernel code outside any task (cleanup at the end of a run).
    Kernel,
}

impl Writer {
    fn current(k: &Kernel) -> Self {
        k.current.get().map_or(Self::Kernel, Self::Task)
    }

    fn label(self, k: &Kernel, slot: u32) -> String {
        match self {
            Self::Task(key) => format!("task `{}`", k.task_name(key)),
            Self::Clock => format!("the clock on `{}`", k.names[slot as usize]),
            Self::Kernel => "the kernel".to_owned(),
        }
    }
}

#[derive(Debug)]
struct Entry {
    slot: u32,
    /// Everyone who wrote the entry's current value this delta.
    writers: Vec<Writer>,
    start: usize,
    len: usize,
}

/// Writes waiting for the next eval, in write order.
#[derive(Debug)]
pub(crate) struct WriteLog {
    entries: Vec<Entry>,
    words: Vec<u32>,
    /// Current delta number; bumped by every `drain`.
    epoch: u64,
    /// Per slot: (epoch of its last write, index into `entries`).
    last: Vec<(u64, usize)>,
}

impl WriteLog {
    pub(crate) fn new(slots: usize) -> Self {
        Self {
            entries: Vec::new(),
            words: Vec::new(),
            epoch: 1,
            last: vec![(0, 0); slots],
        }
    }

    /// Appends a write. Returns another writer of `slot` this delta if it wrote a
    /// different value.
    fn push(&mut self, slot: u32, value: &[u32], writer: Writer) -> Result<(), Writer> {
        let (epoch, index) = self.last[slot as usize];
        if epoch == self.epoch {
            let entry = &mut self.entries[index];
            let current = &mut self.words[entry.start..entry.start + entry.len];
            if current == value {
                if !entry.writers.contains(&writer) {
                    entry.writers.push(writer);
                }
                return Ok(());
            }
            if let Some(&other) = entry.writers.iter().find(|&&w| w != writer) {
                return Err(other);
            }
            current.copy_from_slice(value);
            return Ok(());
        }
        self.last[slot as usize] = (self.epoch, self.entries.len());
        self.entries.push(Entry {
            slot,
            writers: vec![writer],
            start: self.words.len(),
            len: value.len(),
        });
        self.words.extend_from_slice(value);
        Ok(())
    }

    /// Slots with a pending write, in write order.
    pub(crate) fn pending_slots(&self) -> Vec<u32> {
        self.entries.iter().map(|e| e.slot).collect()
    }

    /// Takes every logged write as `(slot, value)` in write order and starts a new delta.
    pub(crate) fn drain(&mut self) -> Vec<(u32, Vec<u32>)> {
        self.epoch += 1;
        let words = std::mem::take(&mut self.words);
        self.entries
            .drain(..)
            .map(|e| (e.slot, words[e.start..e.start + e.len].to_vec()))
            .collect()
    }
}

/// Logs a write by `writer`, enforcing the phase and conflict rules.
pub(crate) fn write(
    k: &Kernel,
    slot: u32,
    value: &[u32],
    writer: Writer,
    location: Option<&'static Location<'static>>,
) {
    let task = match writer {
        Writer::Task(key) => Some(key),
        Writer::Clock | Writer::Kernel => None,
    };
    let name = &k.names[slot as usize];
    match k.phase.get() {
        Phase::Ended => return,
        Phase::ReadOnly => {
            k.fail(
                FailureKind::WriteInReadOnly {
                    signal: name.to_string(),
                },
                task,
                location.map(Into::into),
                format!("signal `{name}` written during the ReadOnly phase"),
            );
            return;
        }
        Phase::Active => {}
    }
    let pushed = k.log.borrow_mut().push(slot, value, writer);
    if let Err(first) = pushed {
        let (first, second) = (first.label(k, slot), writer.label(k, slot));
        k.fail(
            FailureKind::WriteConflict {
                signal: name.to_string(),
                first: first.clone(),
                second: second.clone(),
            },
            task,
            location.map(Into::into),
            format!(
                "signal `{name}` written with different values by {first} and by {second} \
                 in the same delta"
            ),
        );
    }
}
