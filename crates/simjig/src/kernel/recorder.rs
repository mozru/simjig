//! The flight recorder: a bounded ring of the most recent kernel events, plus a running
//! hash of *every* event recorded.
//!
//! The ring answers "what happened just before the failure". The hash is the determinism
//! check (DECISION §2 row 4): two runs of the same test with the same seed must produce the
//! same hash. Records therefore hold only deterministic data (times, slot numbers, task
//! keys from the deterministic slab), never addresses or the global simulation id.

use std::collections::VecDeque;
use std::fmt;

use super::time::SimTime;

/// Records kept in the ring.
pub(crate) const RECORDER_CAPACITY: usize = 4096;

/// What a [`TraceRecord`] describes.
///
/// It may grow: a `match` outside this crate needs a wildcard arm.
///
/// ```compile_fail,E0004
/// fn code(e: &simjig::TraceKind) -> u8 {
///     match e {
///         simjig::TraceKind::Step { .. } => 0,
///         simjig::TraceKind::Eval { .. } => 0,
///         simjig::TraceKind::ReadOnly { .. } => 0,
///         simjig::TraceKind::Spawn { .. } => 0,
///         simjig::TraceKind::Poll { .. } => 0,
///         simjig::TraceKind::Done { .. } => 0,
///         simjig::TraceKind::Cancel { .. } => 0,
///         simjig::TraceKind::Write { .. } => 0,
///         simjig::TraceKind::Failure { .. } => 0,
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraceKind {
    /// The kernel advanced to a new timestamp.
    Step,
    /// The model was evaluated; `arg` is the delta number within the timestep.
    Eval,
    /// The `ReadOnly` phase began.
    ReadOnly,
    /// A task was spawned; `arg` is its parent's task number (`u64::MAX` for none).
    Spawn,
    /// A task was polled.
    Poll,
    /// A task's future completed.
    Done,
    /// A task was cancelled (its future dropped before completing).
    Cancel,
    /// A task's handle was dropped before the task was ever polled, so it never ran (the
    /// `let _ = spawn(..)` mistake), and the run fails; a `Cancel` follows. An explicit
    /// [`Task::cancel`](crate::Task::cancel) is not recorded.
    DroppedUnpolled,
    /// A write was applied to the model; `arg` is the slot, `value` its low 64 bits.
    Write,
    /// The run stopped with a failure.
    Failure,
}

/// One kernel event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecord {
    /// Simulation time of the event.
    pub time: SimTime,
    /// What happened.
    pub kind: TraceKind,
    /// The task involved, as `index << 16 | generation` (`u64::MAX` for none).
    pub task: u64,
    /// Kind-specific argument (see [`TraceKind`]).
    pub arg: u64,
    /// Kind-specific value (see [`TraceKind`]).
    pub value: u64,
}

impl fmt::Display for TraceRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t={} {:?}", self.time.0, self.kind)?;
        if self.task != u64::MAX {
            write!(f, " task={:#x}", self.task)?;
        }
        write!(f, " arg={} value={:#x}", self.arg, self.value)
    }
}

/// FNV-1a, 64-bit: small, dependency-free and stable across platforms and releases,
/// unlike `DefaultHasher`. simjig's one implementation: the trace hash, test-name seeds,
/// random-stream seeds and failure signatures all use it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub(crate) const fn finish(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub(crate) struct Recorder {
    ring: VecDeque<TraceRecord>,
    hash: Fnv,
    count: u64,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            ring: VecDeque::with_capacity(RECORDER_CAPACITY),
            hash: Fnv::default(),
            count: 0,
        }
    }
}

impl Recorder {
    pub(crate) fn record(&mut self, record: TraceRecord) {
        let kind = record.kind as u64;
        for word in [record.time.0, kind, record.task, record.arg, record.value] {
            self.hash.write(&word.to_le_bytes());
        }
        if self.ring.len() == RECORDER_CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(record);
        self.count += 1;
    }

    pub(crate) fn hash(&self) -> u64 {
        self.hash.finish()
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    pub(crate) fn recent(&self) -> Vec<TraceRecord> {
        self.ring.iter().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(time: u64) -> TraceRecord {
        TraceRecord {
            time: SimTime(time),
            kind: TraceKind::Step,
            task: u64::MAX,
            arg: 0,
            value: 0,
        }
    }

    #[test]
    fn fnv_matches_the_reference_vectors() {
        // From the FNV reference test suite (draft-eastlake-fnv, FNV-1a 64).
        let hash = |bytes: &[u8]| {
            let mut h = Fnv::default();
            h.write(bytes);
            h.finish()
        };
        assert_eq!(hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn ring_is_bounded_but_hash_covers_everything() {
        let mut a = Recorder::default();
        let mut b = Recorder::default();
        for t in 0..(RECORDER_CAPACITY as u64 + 10) {
            a.record(rec(t));
            b.record(rec(if t == 0 { 1 } else { t }));
        }
        assert_eq!(a.recent().len(), RECORDER_CAPACITY);
        assert_eq!(
            a.recent(),
            b.recent(),
            "the differing record has left the ring"
        );
        assert_ne!(a.hash(), b.hash(), "but the hash still sees it");
    }
}
