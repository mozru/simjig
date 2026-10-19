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
        let resolved = state::try_current("a signal", |k| {
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
            let buf = [low, high];
            write(
                k,
                slot,
                &buf[..words(width)],
                Writer::current(k),
                Some(location),
            );
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
    #[inline]
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

/// Values of at most this many words are stored in the entry itself.
const INLINE: usize = 2;

#[derive(Debug)]
struct Entry {
    slot: u32,
    len: u32,
    /// The first writer of the entry's current value this delta; writers that agreed with
    /// it are in [`WriteLog::agreeing`].
    writer: Writer,
    /// The value if it fits (`len <= INLINE`); else `[start in WriteLog::words, 0]`.
    value: [u32; INLINE],
}

impl Entry {
    fn value<'a>(&'a self, words: &'a [u32]) -> &'a [u32] {
        let len = self.len as usize;
        if len <= INLINE {
            &self.value[..len]
        } else {
            let start = self.value[0] as usize;
            &words[start..start + len]
        }
    }

    fn value_mut<'a>(&'a mut self, words: &'a mut [u32]) -> &'a mut [u32] {
        let len = self.len as usize;
        if len <= INLINE {
            &mut self.value[..len]
        } else {
            let start = self.value[0] as usize;
            &mut words[start..start + len]
        }
    }
}

/// `dst.copy_from_slice(src)`, without a `memcpy` call for the usual one or two words.
#[inline]
fn copy_small(dst: &mut [u32], src: &[u32]) {
    match (dst, src) {
        ([d], [s]) => *d = *s,
        ([d0, d1], [s0, s1]) => {
            *d0 = *s0;
            *d1 = *s1;
        }
        (dst, src) => dst.copy_from_slice(src),
    }
}

/// Writes waiting for the next eval, in write order.
///
/// The hot path (one write of a slot not yet written this delta) allocates nothing once
/// the buffers have grown: entries and words are kept across deltas
/// ([`clear`](Self::clear)).
#[derive(Debug)]
pub(crate) struct WriteLog {
    entries: Vec<Entry>,
    words: Vec<u32>,
    /// Current delta number, never 0; bumped by every `take`.
    epoch: u32,
    /// Per slot: (epoch of its last write, index into `entries`). Eight bytes, so the
    /// lookup is one load; `take` clears it when the epoch wraps.
    last: Vec<(u32, u32)>,
    /// `(entry index, writer)` of every further writer that wrote an entry's current
    /// value this delta; rare (agreeing writers), so kept out of the entries.
    agreeing: Vec<(usize, Writer)>,
}

impl WriteLog {
    pub(crate) fn new(slots: usize) -> Self {
        Self {
            entries: Vec::new(),
            words: Vec::new(),
            epoch: 1,
            last: vec![(0, 0); slots],
            agreeing: Vec::new(),
        }
    }

    /// Appends a write. Returns another writer of `slot` this delta if it wrote a
    /// different value.
    ///
    /// The two common cases (the first write of a slot this delta, and its only writer
    /// writing it again) are inline and short; agreeing and conflicting writers take the
    /// out-of-line [`push_shared`](Self::push_shared).
    #[inline]
    fn push(&mut self, slot: u32, value: &[u32], writer: Writer) -> Result<(), Writer> {
        let (epoch, index) = self.last[slot as usize];
        if epoch != self.epoch {
            self.push_new(slot, value, writer);
            return Ok(());
        }
        let entry = &mut self.entries[index as usize];
        if entry.writer == writer && self.agreeing.is_empty() {
            // The last write wins; there is no one to conflict with.
            copy_small(entry.value_mut(&mut self.words), value);
            return Ok(());
        }
        self.push_shared(index as usize, value, writer)
    }

    #[inline]
    fn push_new(&mut self, slot: u32, value: &[u32], writer: Writer) {
        let entry = u32::try_from(self.entries.len()).or_invariant("invariant: < 2^32 writes");
        self.last[slot as usize] = (self.epoch, entry);
        let len = u32::try_from(value.len()).or_invariant("invariant: < 2^32 words");
        // Most signals fit in the entry, which saves a second buffer and a `memcpy`.
        let inline = match *value {
            [w] => [w, 0],
            [w0, w1] => [w0, w1],
            _ => self.spill(value),
        };
        self.entries.push(Entry {
            slot,
            len,
            writer,
            value: inline,
        });
    }

    /// Stores a value too wide for an entry; returns the entry's `value` field.
    #[cold]
    fn spill(&mut self, value: &[u32]) -> [u32; INLINE] {
        let start = u32::try_from(self.words.len()).or_invariant("invariant: < 2^32 words");
        self.words.extend_from_slice(value);
        [start, 0]
    }

    /// A write of an entry that another writer wrote, or that agreeing writers share.
    #[cold]
    #[inline(never)]
    fn push_shared(&mut self, index: usize, value: &[u32], writer: Writer) -> Result<(), Writer> {
        let entry = &mut self.entries[index];
        let first = entry.writer;
        let current = entry.value_mut(&mut self.words);
        let mut others = self
            .agreeing
            .iter()
            .filter(|(i, _)| *i == index)
            .map(|&(_, w)| w);
        if current == value {
            if first != writer && !others.any(|w| w == writer) {
                self.agreeing.push((index, writer));
            }
            return Ok(());
        }
        if first != writer {
            return Err(first);
        }
        if let Some(other) = others.find(|&w| w != writer) {
            return Err(other);
        }
        copy_small(current, value);
        Ok(())
    }

    /// Whether nothing was logged since the last `take`.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Slots with a pending write, in write order.
    pub(crate) fn pending_slots(&self) -> Vec<u32> {
        self.entries.iter().map(|e| e.slot).collect()
    }

    /// `(slot, value)` of every logged write, in write order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &[u32])> {
        self.entries.iter().map(|e| (e.slot, e.value(&self.words)))
    }

    /// Forgets every logged write (they were applied) and starts a new delta.
    pub(crate) fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            // Wrapped: forget every slot's epoch so no stale one can match.
            self.last.fill((0, 0));
            self.epoch = 1;
        }
        self.agreeing.clear();
        self.entries.clear();
        self.words.clear();
    }
}

/// Logs a write by `writer`, enforcing the phase and conflict rules.
///
/// `#[inline]` here and on the log's `push`: the hot path is a few loads and two vector
/// pushes, and the call is made from `RawSig` methods inlined into the user's crate.
#[inline]
pub(crate) fn write(
    k: &Kernel,
    slot: u32,
    value: &[u32],
    writer: Writer,
    location: Option<&'static Location<'static>>,
) {
    match k.phase.get() {
        Phase::Active => {}
        Phase::Ended => return,
        Phase::ReadOnly => return write_in_read_only(k, slot, writer, location),
        // No log: the model is never mid-eval while the testbench runs, and the next eval
        // takes the value (`kernel::sync_clock`).
        Phase::Sync => {
            if !k.direct.write(slot, value) {
                k.models.borrow_mut().write(slot, value);
            }
            return;
        }
    }
    let pushed = k.log.borrow_mut().push(slot, value, writer);
    if let Err(first) = pushed {
        conflict(k, slot, first, writer, location);
    }
}

const fn task_of(writer: Writer) -> Option<Key> {
    match writer {
        Writer::Task(key) => Some(key),
        Writer::Clock | Writer::Kernel => None,
    }
}

#[cold]
fn write_in_read_only(
    k: &Kernel,
    slot: u32,
    writer: Writer,
    location: Option<&'static Location<'static>>,
) {
    let name = &k.names[slot as usize];
    k.fail(
        FailureKind::WriteInReadOnly {
            signal: name.to_string(),
        },
        task_of(writer),
        location.map(Into::into),
        format!("signal `{name}` written during the ReadOnly phase"),
    );
}

#[cold]
fn conflict(
    k: &Kernel,
    slot: u32,
    first: Writer,
    second: Writer,
    location: Option<&'static Location<'static>>,
) {
    let name = &k.names[slot as usize];
    let (first, second_label) = (first.label(k, slot), second.label(k, slot));
    k.fail(
        FailureKind::WriteConflict {
            signal: name.to_string(),
            first: first.clone(),
            second: second_label.clone(),
        },
        task_of(second),
        location.map(Into::into),
        format!(
            "signal `{name}` written with different values by {first} and by {second_label} \
             in the same delta"
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(index: u32) -> Writer {
        Writer::Task(Key { index, gen_: 0 })
    }

    #[test]
    fn the_log_keeps_the_last_value_and_names_a_disagreeing_writer() {
        let (a, b, c) = (task(1), task(2), task(3));
        let mut log = WriteLog::new(2);
        assert_eq!(log.push(0, &[5], a), Ok(()));
        assert_eq!(log.push(0, &[6], a), Ok(()), "one writer: last write wins");
        assert_eq!(
            log.push(0, &[6], b),
            Ok(()),
            "agreeing writers are accepted"
        );
        // `a` changing its value now conflicts with `b`, who agreed on 6.
        assert_eq!(log.push(0, &[7], a), Err(b));
        assert_eq!(log.push(0, &[7], b), Err(a));
        assert_eq!(log.push(0, &[8], c), Err(a));
        assert_eq!(log.push(1, &[1], Writer::Clock), Ok(()));
        let applied: Vec<(u32, Vec<u32>)> = log.iter().map(|(s, v)| (s, v.to_vec())).collect();
        assert_eq!(applied, [(0, vec![6]), (1, vec![1])]);
        log.clear();
        // A new delta: the old writers no longer count.
        assert_eq!(log.push(0, &[9], c), Ok(()));
        assert_eq!(log.iter().count(), 1);
    }
}
