//! The flight recorder: a bounded ring of the most recent kernel events, plus a running
//! hash of *every* event recorded.
//!
//! The ring answers "what happened just before the failure". The hash is the determinism
//! check (DECISION §2 row 4): two runs of the same test with the same seed must produce the
//! same hash. Records therefore hold only deterministic data (times, slot numbers, task
//! keys from the deterministic slab), never addresses or the global simulation id.

use std::cell::Cell;

use super::OrInvariant;
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
    /// `let _ = spawn(..)` mistake), and the run fails unless `main` returned in the same
    /// poll; a `Cancel` follows. An explicit
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
/// unlike `DefaultHasher`. simjig's one implementation for hashing names: test-name seeds,
/// random-stream seeds and failure signatures use it. (The trace hash is [`TraceHash`].)
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(OFFSET_BASIS)
    }
}

/// The FNV-1a 64 offset basis: also the trace hash of a run that recorded nothing.
const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// The FNV-1a 64 prime.
const PRIME: u64 = 0x0000_0100_0000_01b3;

impl Fnv {
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(PRIME);
        }
    }

    pub(crate) const fn finish(self) -> u64 {
        self.0
    }
}

/// The 64 high and 64 low bits of `a * b`, xored: one full-width multiply that mixes every
/// bit of both operands into every bit of the result (wyhash's `mum`).
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the two halves of the 128-bit product"
)]
const fn fold(a: u64, b: u64) -> u64 {
    let p = (a as u128).wrapping_mul(b as u128);
    (p as u64) ^ ((p >> 64) as u64)
}

/// Two odd constants with no structure (wyhash's first secrets): the keys xored into the
/// [`fold`]s' operands, so that no operand is zero for the small words records hold, and
/// `K0` is also the chain's multiplier. Two, not one per word: each costs four
/// instructions to materialise at every record.
const K0: u64 = 0xa076_1d64_78bd_642f;
const K1: u64 = 0xe703_7ed1_a0b4_28db;

/// The word a record's kind is xored into its value with: `kind + 1` in the top byte, one
/// instruction to materialise. A value equal to it would zero the second [`fold`]'s
/// operand, so the argument is also xored into the digest directly.
const fn kind_tag(kind: TraceKind) -> u64 {
    (kind as u64 + 1) << 56
}

/// The trace hash: a running hash of every [`TraceRecord`], in order (DECISION §2 row 4:
/// two runs of the same test with the same seed must give the same value). Stable across
/// platforms and releases: integer arithmetic on the records' words only.
///
/// Each record's five words are first reduced to one *digest*, independent of the running
/// state: `fold(time ^ K0, task ^ K1) ^ fold(arg ^ K1, value ^ kind_tag(kind)) ^ arg`. The
/// state then takes it as `(h ^ digest).rotate_left(23) * K0` (wrapping), a bijection of
/// the state for each digest; it starts at the FNV-1a offset basis. So
/// the serial dependency from one record to the next is three instructions, and the
/// digest's work overlaps the rest of the scheduler.
///
/// Why not FNV-1a over the records' bytes, as before: that is 40 dependent byte steps per
/// record, about a tenth of a simulated cycle's instructions on a small design (17
/// records per cycle of the benchmark's FIFO testbench, `docs/notes/perf.md`). The values
/// differ from that hash's (a breaking change of every recorded hash, in the CHANGELOG);
/// the names' hash ([`Fnv`]) is unchanged, so seeds and failure signatures are too.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TraceHash(u64);

impl Default for TraceHash {
    fn default() -> Self {
        Self(OFFSET_BASIS)
    }
}

/// The first half of a record's digest: its time and task.
#[inline]
const fn time_task(time: u64, task: u64) -> u64 {
    fold(time ^ K0, task ^ K1)
}

impl TraceHash {
    /// Takes `record` into the state, given `time_task(record.time, record.task)`.
    #[inline]
    fn add(&mut self, time_task: u64, record: &TraceRecord) {
        let digest =
            time_task ^ fold(record.arg ^ K1, record.value ^ kind_tag(record.kind)) ^ record.arg;
        self.0 = (self.0 ^ digest).rotate_left(23).wrapping_mul(K0);
    }

    pub(crate) const fn finish(self) -> u64 {
        self.0
    }
}

const _: () = assert!(
    (TraceKind::Failure as u64) < 255,
    "every kind has its own tag byte"
);

/// A fixed ring of [`RECORDER_CAPACITY`] records: record `i` (counting from 0) is at
/// `ring[i % RECORDER_CAPACITY]`, so a record is one store and a count, with no length
/// bookkeeping.
///
/// Every field is a `Cell`, so recording takes `&self`: the kernel records several times
/// per timestep, and a `RefCell` around the recorder cost a borrow check each time.
/// Nothing borrows into it, and recording calls no other code, so no record can interleave
/// with another.
#[derive(Debug)]
pub(crate) struct Recorder {
    ring: Box<[Cell<TraceRecord>; RECORDER_CAPACITY]>,
    hash: Cell<TraceHash>,
    /// Whether records are hashed (the default); off only for a run that asked to keep no
    /// trace hash (`#[simjig::test(trace_hash = false)]`).
    hashing: Cell<bool>,
    count: Cell<u64>,
    /// A time and its `time_task` with no task: most records have no task (a timestep's
    /// `Step`, `Eval`s, `ReadOnly` and `Write`s), and all of a timestep's share its time.
    untasked: Cell<(u64, u64)>,
}

const _: () = assert!(
    RECORDER_CAPACITY.is_power_of_two(),
    "the ring index is a mask"
);

impl Default for Recorder {
    fn default() -> Self {
        let blank = TraceRecord {
            time: SimTime(0),
            kind: TraceKind::Step,
            task: u64::MAX,
            arg: 0,
            value: 0,
        };
        let ring: Box<[Cell<TraceRecord>]> =
            (0..RECORDER_CAPACITY).map(|_| Cell::new(blank)).collect();
        Self {
            ring: ring
                .try_into()
                .or_invariant("invariant: the ring has RECORDER_CAPACITY records"),
            hash: Cell::new(TraceHash::default()),
            hashing: Cell::new(true),
            count: Cell::new(0),
            untasked: Cell::new((0, time_task(0, u64::MAX))),
        }
    }
}

impl Recorder {
    /// Records `record`: hashes it (unless hashing stopped) and keeps it in the ring.
    /// Inlined, so a record whose words are constants (a `Step`, an `Eval`) has its digest
    /// partly folded at compile time.
    #[inline]
    pub(crate) fn record(&self, record: TraceRecord) {
        if self.hashing.get() {
            let time = record.time.0;
            let first = if record.task == u64::MAX {
                let (at, cached) = self.untasked.get();
                if at == time {
                    cached
                } else {
                    let first = time_task(time, u64::MAX);
                    self.untasked.set((time, first));
                    first
                }
            } else {
                time_task(time, record.task)
            };
            let mut hash = self.hash.get();
            hash.add(first, &record);
            self.hash.set(hash);
        }
        self.store(record);
    }

    /// Stops hashing: later records are kept in the ring and counted, not hashed.
    pub(crate) fn stop_hashing(&self) {
        self.hashing.set(false);
    }

    /// The hash of every record, if every record was hashed.
    pub(crate) fn full_hash(&self) -> Option<u64> {
        self.hashing.get().then(|| self.hash())
    }

    #[inline]
    fn store(&self, record: TraceRecord) {
        let count = self.count.get();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the index is masked to the ring's size"
        )]
        let at = count as usize & (RECORDER_CAPACITY - 1);
        self.ring[at].set(record);
        self.count.set(count + 1);
    }

    pub(crate) fn hash(&self) -> u64 {
        self.hash.get().finish()
    }

    pub(crate) fn count(&self) -> u64 {
        self.count.get()
    }

    /// The kept records, oldest first.
    pub(crate) fn recent(&self) -> Vec<TraceRecord> {
        let count = self.count.get();
        let next = usize::try_from(count % RECORDER_CAPACITY as u64).unwrap_or(0);
        let (older, newer) = if count >= RECORDER_CAPACITY as u64 {
            (&self.ring[next..], &self.ring[..next])
        } else {
            (&self.ring[..next], &self.ring[..0])
        };
        older.iter().chain(newer).map(Cell::get).collect()
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

    /// [`TraceHash`] of the records below, computed independently (Python, from the
    /// definition in its docs).
    const STABLE_HASH: u64 = 0x463d_2eed_6aa0_93a1;

    /// The trace hash of a fixed record sequence: replays compare such values across
    /// processes and versions, so a change of the hash must be deliberate.
    #[test]
    fn the_trace_hash_is_stable() {
        let r = Recorder::default();
        for t in 0..100u64 {
            r.record(TraceRecord {
                time: SimTime(t * 5000),
                kind: if t % 3 == 0 {
                    TraceKind::Eval
                } else {
                    TraceKind::Write
                },
                task: if t % 2 == 0 { u64::MAX } else { t << 16 | 1 },
                arg: t % 7,
                value: t * 0x0101,
            });
        }
        assert_eq!(r.hash(), STABLE_HASH);
    }

    /// The hash sees each word of a record, and the records' order.
    #[test]
    fn the_trace_hash_sees_every_word_and_the_order() {
        let base = TraceRecord {
            time: SimTime(5000),
            kind: TraceKind::Write,
            task: 3 << 16 | 1,
            arg: 2,
            value: 0x5a,
        };
        let hash = |records: &[TraceRecord]| {
            let r = Recorder::default();
            for &x in records {
                r.record(x);
            }
            r.hash()
        };
        let variants = [
            TraceRecord {
                time: SimTime(5001),
                ..base
            },
            TraceRecord {
                kind: TraceKind::Poll,
                ..base
            },
            TraceRecord {
                task: u64::MAX,
                ..base
            },
            TraceRecord { arg: 3, ..base },
            TraceRecord {
                value: 0x5b,
                ..base
            },
        ];
        let one = hash(&[base]);
        for v in variants {
            assert_ne!(hash(&[v]), one, "{v:?}");
            assert_ne!(hash(&[base, v]), hash(&[v, base]), "order of {v:?}");
        }
        assert_eq!(hash(&[]), OFFSET_BASIS, "nothing recorded");
    }

    #[test]
    fn a_recorder_that_stops_hashing_keeps_recording_but_has_no_full_hash() {
        let r = Recorder::default();
        r.record(rec(1));
        assert_eq!(r.full_hash(), Some(r.hash()));
        r.stop_hashing();
        let before = r.hash();
        r.record(rec(2));
        assert_eq!(r.hash(), before, "not hashed");
        assert_eq!(r.full_hash(), None);
        assert_eq!(r.count(), 2);
        assert_eq!(r.recent().len(), 2);
    }

    #[test]
    fn the_ring_keeps_the_newest_records_in_order() {
        let r = Recorder::default();
        for t in 0..3 {
            r.record(rec(t));
        }
        let times = |r: &Recorder| r.recent().iter().map(|x| x.time.0).collect::<Vec<_>>();
        assert_eq!(times(&r), [0, 1, 2]);
        for t in 3..(RECORDER_CAPACITY as u64 + 5) {
            r.record(rec(t));
        }
        let kept = times(&r);
        assert_eq!(kept.len(), RECORDER_CAPACITY);
        assert_eq!(kept[0], 5);
        assert!(kept.windows(2).all(|w| w[1] == w[0] + 1));
    }

    #[test]
    fn ring_is_bounded_but_hash_covers_everything() {
        let a = Recorder::default();
        let b = Recorder::default();
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
