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

/// The word a record's kind is xored into its digest with: `kind + 1` in the top byte,
/// one instruction to materialise.
const fn kind_tag(kind: TraceKind) -> u64 {
    (kind as u64 + 1) << 56
}

/// The second half of a record's digest: `fold(arg ^ K1, value ^ K0) ^ arg ^ kind_tag`
/// (`k0`, `k1` are [`K0`], [`K1`]).
///
/// The kind is xored in outside the fold, not into the value: xored into the value, two
/// records whose values differ by their kinds' tags hashed alike, and with a value of 0
/// the multiplier was the tag alone, under which an argument and its complement hashed
/// alike (a `Spawn` by `main`, parent 0, against one with no parent, `u64::MAX`). Keyed
/// with `K0`, the multiplier is zero only for the value `K0`, and then the argument and
/// kind are still in the digest. The other operand is zero only for an argument of `K1`,
/// which no record has: an argument is below 2^32 or `u64::MAX` ([`Kept`]).
#[inline]
const fn second_half(tag: u64, arg: u64, value: u64, k0: u64, k1: u64) -> u64 {
    fold(arg ^ k1, value ^ k0) ^ arg ^ tag
}

/// The trace hash: a running hash of every [`TraceRecord`], in order (DECISION §2 row 4:
/// two runs of the same test with the same seed must give the same value). Stable across
/// platforms and releases: integer arithmetic on the records' words only.
///
/// Each record's five words are first reduced to one *digest*, independent of the running
/// state: `fold(time ^ K0, task ^ K1) ^ fold(arg ^ K1, value ^ K0) ^ arg ^ kind_tag(kind)`
/// ([`second_half`]). The state then takes it as `(h ^ digest).rotate_left(23) * K0`
/// (wrapping), a bijection of the state for each digest; it starts at the FNV-1a offset
/// basis. So the serial dependency from one record to the next is three instructions, and the
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

impl TraceHash {
    /// Takes a record's digest into the state; `k0` is [`K0`].
    #[inline]
    fn add(&mut self, digest: u64, k0: u64) {
        self.0 = (self.0 ^ digest).rotate_left(23).wrapping_mul(k0);
    }

    pub(crate) const fn finish(self) -> u64 {
        self.0
    }
}

const _: () = assert!(
    (TraceKind::Failure as u64) < 255,
    "every kind has its own tag byte"
);

/// A [`TraceRecord`] as the ring keeps it: 32 bytes, not 40, so the ring (written on every
/// record, read only for a report) takes less of the cache. A record's task and argument
/// are below 2^32 or `u64::MAX` ("none"): a task is `index << 16 | generation` with an
/// index below 2^16, an argument a slot, a delta, a task index or a flag. Each is kept in
/// 32 bits, with a bit of `none` for `u64::MAX`.
#[derive(Clone, Copy, Debug)]
struct Kept {
    time: SimTime,
    value: u64,
    task: u32,
    arg: u32,
    kind: TraceKind,
    /// Bit 0: the task is `u64::MAX`; bit 1: the argument is.
    none: u8,
}

/// `word`'s low 32 bits, and whether it is `u64::MAX` ([`Kept`]).
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a record's task and argument fit 32 bits (`Kept`)"
)]
fn narrow(word: u64) -> (u32, bool) {
    debug_assert!(
        word == u64::MAX || u32::try_from(word).is_ok(),
        "invariant: a record's task or argument fits 32 bits"
    );
    (word as u32, word == u64::MAX)
}

/// [`narrow`] undone.
fn widen(word: u32, none: bool) -> u64 {
    if none { u64::MAX } else { u64::from(word) }
}

impl From<TraceRecord> for Kept {
    #[inline]
    fn from(r: TraceRecord) -> Self {
        let (task, no_task) = narrow(r.task);
        let (arg, no_arg) = narrow(r.arg);
        Self {
            time: r.time,
            value: r.value,
            task,
            arg,
            kind: r.kind,
            none: u8::from(no_task) | u8::from(no_arg) << 1,
        }
    }
}

impl From<Kept> for TraceRecord {
    fn from(k: Kept) -> Self {
        Self {
            time: k.time,
            kind: k.kind,
            task: widen(k.task, k.none & 1 != 0),
            arg: widen(k.arg, k.none & 2 != 0),
            value: k.value,
        }
    }
}

const _: () = assert!(size_of::<Kept>() == 32, "a kept record is 32 bytes");

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
    ring: Box<[Cell<Kept>; RECORDER_CAPACITY]>,
    hash: Cell<TraceHash>,
    /// Whether records are hashed (the default); off only for a run that asked to keep no
    /// trace hash (`#[simjig::test(trace_hash = false)]`).
    hashing: Cell<bool>,
    count: Cell<u64>,
    /// A time and the first half of its digest with no task (`fold(time ^ K0, task ^ K1)`):
    /// most records have no task (a timestep's `Step`, `Eval`s, `ReadOnly` and `Write`s),
    /// and all of a timestep's share its time.
    untasked: Cell<(u64, u64)>,
    keys: Keys,
}

/// The trace hash's constants, kept as data: a 64-bit constant written into the code
/// takes four instructions at every record, a load from here one. Never changed.
#[derive(Debug)]
struct Keys {
    k0: u64,
    k1: u64,
    /// The second half of the digest ([`second_half`]) of each [`Fixed`] record, in its
    /// order.
    fixed: [u64; FIXED],
}

/// A record whose words after its time are always the same: the timestep's `Step`,
/// `ReadOnly` and first evals ([`Recorder::record_fixed`]).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fixed {
    Step,
    ReadOnly,
    /// Eval number `n` of the timestep, for `n` below [`FIXED_EVALS`].
    Eval(u8),
}

/// The evals of a timestep that are [`Fixed`] records.
pub(crate) const FIXED_EVALS: u8 = 4;

const FIXED: usize = 2 + FIXED_EVALS as usize;

impl Fixed {
    /// The record's kind and argument (its task is none and its value 0).
    const fn words(self) -> (TraceKind, u64) {
        match self {
            Self::Step => (TraceKind::Step, 0),
            Self::ReadOnly => (TraceKind::ReadOnly, 0),
            Self::Eval(n) => (TraceKind::Eval, n as u64),
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Step => 0,
            Self::ReadOnly => 1,
            Self::Eval(n) => 2 + n as usize,
        }
    }
}

impl Default for Keys {
    fn default() -> Self {
        let mut fixed = [0; FIXED];
        for f in [Fixed::Step, Fixed::ReadOnly]
            .into_iter()
            .chain((0..FIXED_EVALS).map(Fixed::Eval))
        {
            let (kind, arg) = f.words();
            fixed[f.index()] = second_half(kind_tag(kind), arg, 0, K0, K1);
        }
        Self {
            k0: K0,
            k1: K1,
            fixed,
        }
    }
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
        let ring: Box<[Cell<Kept>]> = (0..RECORDER_CAPACITY)
            .map(|_| Cell::new(Kept::from(blank)))
            .collect();
        Self {
            ring: ring
                .try_into()
                .or_invariant("invariant: the ring has RECORDER_CAPACITY records"),
            hash: Cell::new(TraceHash::default()),
            hashing: Cell::new(true),
            count: Cell::new(0),
            untasked: Cell::new((0, fold(K0, u64::MAX ^ K1))),
            keys: Keys::default(),
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
            let Keys { k0, k1, .. } = self.keys;
            let time = record.time.0;
            let first = if record.task == u64::MAX {
                self.untasked(time)
            } else {
                fold(time ^ k0, record.task ^ k1)
            };
            let tag = kind_tag(record.kind);
            let second = second_half(tag, record.arg, record.value, k0, k1);
            self.add(first ^ second);
        }
        self.store(record);
    }

    /// Records the [`Fixed`] record `fixed` at `time`: the same hash as
    /// [`record`](Self::record), with the second half of its digest looked up.
    #[inline]
    pub(crate) fn record_fixed(&self, time: SimTime, fixed: Fixed) {
        if self.hashing.get() {
            let second = self.keys.fixed[fixed.index()];
            self.add(self.untasked(time.0) ^ second);
        }
        let (kind, arg) = fixed.words();
        self.store(TraceRecord {
            time,
            kind,
            task: u64::MAX,
            arg,
            value: 0,
        });
    }

    /// The first half of the digest of a record at `time` with no task.
    #[inline]
    fn untasked(&self, time: u64) -> u64 {
        let (at, cached) = self.untasked.get();
        if at == time {
            return cached;
        }
        let first = fold(time ^ self.keys.k0, u64::MAX ^ self.keys.k1);
        self.untasked.set((time, first));
        first
    }

    /// Takes a record's digest into the hash.
    #[inline]
    fn add(&self, digest: u64) {
        let mut hash = self.hash.get();
        hash.add(digest, self.keys.k0);
        self.hash.set(hash);
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
        self.ring[at].set(Kept::from(record));
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
        older
            .iter()
            .chain(newer)
            .map(|k| TraceRecord::from(k.get()))
            .collect()
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
    const STABLE_HASH: u64 = 0xf7ff_8eab_73a2_7236;

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

    /// A fixed record hashes and keeps exactly what the same record given in full does.
    #[test]
    fn a_fixed_record_is_the_record_it_stands_for() {
        let fixed = [Fixed::Step, Fixed::ReadOnly]
            .into_iter()
            .chain((0..FIXED_EVALS).map(Fixed::Eval));
        for f in fixed {
            let (plain, short) = (Recorder::default(), Recorder::default());
            for (i, t) in [0u64, 0, 5000, 5000, 7, 10_000].into_iter().enumerate() {
                let (kind, arg) = f.words();
                plain.record(TraceRecord {
                    time: SimTime(t),
                    kind,
                    task: u64::MAX,
                    arg,
                    value: 0,
                });
                short.record_fixed(SimTime(t), f);
                assert_eq!(short.hash(), plain.hash(), "{f:?} record {i}");
            }
            assert_eq!(short.recent(), plain.recent(), "{f:?}");
        }
    }

    /// The ring gives back every record as it was given, at the edges of the 32-bit
    /// fields it keeps tasks and arguments in.
    #[test]
    fn the_ring_keeps_tasks_and_arguments_exactly() {
        let words = [0, 1, 0xffff, 0xffff_fffe, u64::from(u32::MAX), u64::MAX];
        let r = Recorder::default();
        let mut given = Vec::new();
        for (i, &task) in words.iter().enumerate() {
            for &arg in &words {
                let record = TraceRecord {
                    time: SimTime(i as u64),
                    kind: TraceKind::Spawn,
                    task,
                    arg,
                    value: u64::MAX - arg,
                };
                r.record(record);
                given.push(record);
            }
        }
        assert_eq!(r.recent(), given);
    }

    /// A `Spawn` by `main` (parent index 0) and one with no parent (`u64::MAX`) are
    /// different runs: with value 0 the second fold's multiplier was once the kind tag
    /// alone, under which an argument and its complement hashed alike.
    #[test]
    fn an_argument_and_its_complement_hash_apart() {
        let hash = |arg: u64| {
            let r = Recorder::default();
            r.record(TraceRecord {
                time: SimTime(0),
                kind: TraceKind::Spawn,
                task: 1 << 16 | 1,
                arg,
                value: 0,
            });
            r.hash()
        };
        assert_ne!(hash(0), hash(u64::MAX), "parent main vs no parent");
    }

    /// A record's kind and its value are separate words: two records whose values differ
    /// by exactly their kinds' tags (a `Write` of the `Write` tag itself against a `Poll`
    /// of the `Poll` tag, say) are different runs.
    #[test]
    fn a_kind_and_a_value_hash_apart() {
        let hash = |kind: TraceKind, value: u64| {
            let r = Recorder::default();
            r.record(TraceRecord {
                time: SimTime(5000),
                kind,
                task: 3 << 16 | 1,
                arg: 2,
                value,
            });
            r.hash()
        };
        let (write, poll) = (kind_tag(TraceKind::Write), kind_tag(TraceKind::Poll));
        assert_ne!(hash(TraceKind::Write, write), hash(TraceKind::Poll, poll));
        assert_ne!(
            hash(TraceKind::Write, 0),
            hash(TraceKind::Poll, write ^ poll)
        );
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
