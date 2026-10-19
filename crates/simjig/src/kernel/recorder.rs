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
/// unlike `DefaultHasher`. simjig's one implementation: the trace hash, test-name seeds,
/// random-stream seeds and failure signatures all use it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

/// The FNV-1a 64 prime.
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// `PRIME_POW[n]` is `PRIME` to the `n`th power (wrapping): `n` zero bytes in a row.
const PRIME_POW: [u64; 9] = {
    let mut pow = [1u64; 9];
    let mut n = 1;
    while n < 9 {
        pow[n] = pow[n - 1].wrapping_mul(PRIME);
        n += 1;
    }
    pow
};

/// `PRIME` to the `n`th power (wrapping).
const fn prime_pow(n: usize) -> u64 {
    let (mut pow, mut i) = (1u64, 0);
    while i < n {
        pow = pow.wrapping_mul(PRIME);
        i += 1;
    }
    pow
}

/// Hashing a fixed byte string `bytes` from state `h` gives
/// `h * PRIME^N + table[h & 0xff]` (wrapping); this is that table.
///
/// Why: write `h = q + l` with `l = h & 0xff`. A byte `b` changes only the low byte, so
/// `(h ^ b) * PRIME = h * PRIME + ((l ^ b) - l) * PRIME`, and the next low byte is
/// `((l ^ b) * PRIME) & 0xff`, which depends on `l` alone. Over a fixed byte string, the
/// sum of those corrections (each multiplied on by the rest of the string) is therefore a
/// function of the first low byte: one table lookup and one multiplication instead of `N`
/// dependent steps.
const fn fixed_string_table<const N: usize>(bytes: [u8; N]) -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut l0 = 0;
    while l0 < 256 {
        let (mut l, mut sum) = (l0 as u64, 0u64);
        let mut i = 0;
        while i < N {
            let b = bytes[i] as u64;
            let d = (l ^ b).wrapping_sub(l);
            sum = sum.wrapping_add(d.wrapping_mul(prime_pow(N - i)));
            l = ((l ^ b).wrapping_mul(PRIME)) & 0xff;
            i += 1;
        }
        table[l0] = sum;
        l0 += 1;
    }
    table
}

/// `PRIME^32`: a record's four words after its time.
const PRIME_POW_32: u64 = prime_pow(32);

/// The eight bytes `0xff` of an all-ones word (a record with no task).
const ALL_ONES: [u64; 256] = fixed_string_table([0xff; 8]);

/// What follows the time in a record of `kind` with no task, argument `arg` and value 0:
/// the little-endian bytes of `kind`, `u64::MAX`, `arg` and 0.
const fn record_tail(kind: u8, arg: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = kind;
    let mut i = 8;
    while i < 16 {
        bytes[i] = 0xff;
        i += 1;
    }
    bytes[16] = arg;
    bytes
}

/// The tail of a record that is the same in every timestep, hashed with one lookup
/// ([`fixed_string_table`]): a `Step`, a `ReadOnly`, and the first evals of a timestep.
#[derive(Debug)]
pub(crate) struct FixedTail {
    table: [u64; 256],
}

impl FixedTail {
    const fn new(kind: TraceKind, arg: u8) -> Self {
        Self {
            table: fixed_string_table(record_tail(kind as u8, arg)),
        }
    }
}

/// `Step` (no task, argument 0, value 0).
pub(crate) static STEP_TAIL: FixedTail = FixedTail::new(TraceKind::Step, 0);
/// `ReadOnly` (no task, argument 0, value 0).
pub(crate) static READ_ONLY_TAIL: FixedTail = FixedTail::new(TraceKind::ReadOnly, 0);
/// `Eval` of delta `n` (no task, value 0), for the first [`EVAL_TAILS`]`.len()` deltas.
pub(crate) static EVAL_TAILS: [FixedTail; 4] = [
    FixedTail::new(TraceKind::Eval, 0),
    FixedTail::new(TraceKind::Eval, 1),
    FixedTail::new(TraceKind::Eval, 2),
    FixedTail::new(TraceKind::Eval, 3),
];

impl Fnv {
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(PRIME);
        }
    }

    /// The same as `write(&word.to_le_bytes())`, faster for small words and all-ones.
    ///
    /// A zero byte's step is `(h ^ 0) * PRIME = h * PRIME`, so the run of zero bytes at the
    /// top of a little-endian word is one multiplication by `PRIME_POW[run]`. The trace
    /// hash's words are mostly small (kinds, slot numbers, deltas, zero values), so most of
    /// a record's 40 serial multiplications go; every hash value stays the same.
    #[inline]
    pub(crate) fn write_u64(&mut self, word: u64) {
        if word == u64::MAX {
            let h = self.0;
            self.0 = h
                .wrapping_mul(PRIME_POW[8])
                .wrapping_add(ALL_ONES[(h & 0xff) as usize]);
            return;
        }
        let bytes = 8 - (word.leading_zeros() / 8) as usize;
        let mut h = self.0;
        for i in 0..bytes {
            h = (h ^ ((word >> (8 * i)) & 0xff)).wrapping_mul(PRIME);
        }
        self.0 = h.wrapping_mul(PRIME_POW[8 - bytes]);
    }

    pub(crate) const fn finish(self) -> u64 {
        self.0
    }
}

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
    hash: Cell<Fnv>,
    /// Whether records are hashed (the default); off only for a run that asked to keep no
    /// trace hash (`#[simjig::test(trace_hash = false)]`).
    hashing: Cell<bool>,
    count: Cell<u64>,
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
            hash: Cell::new(Fnv::default()),
            hashing: Cell::new(true),
            count: Cell::new(0),
        }
    }
}

impl Recorder {
    pub(crate) fn record(&self, record: TraceRecord) {
        if self.hashing.get() {
            self.hash_record(&record);
        }
        self.store(record);
    }

    /// Records `record`, whose words after the time are the fixed string `tail` stands for:
    /// the same hash as [`record`](Self::record), with the tail in one step.
    #[inline]
    pub(crate) fn record_fixed(&self, record: TraceRecord, tail: &FixedTail) {
        if self.hashing.get() {
            let mut hash = self.hash.get();
            hash.write_u64(record.time.0);
            let h = hash.0;
            hash.0 = h
                .wrapping_mul(PRIME_POW_32)
                .wrapping_add(tail.table[(h & 0xff) as usize]);
            self.hash.set(hash);
        }
        self.store(record);
    }

    fn hash_record(&self, record: &TraceRecord) {
        let mut hash = self.hash.get();
        // The bytes hashed are the five words' little-endian bytes, in this order.
        for word in [
            record.time.0,
            record.kind as u64,
            record.task,
            record.arg,
            record.value,
        ] {
            hash.write_u64(word);
        }
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

    #[test]
    fn write_u64_hashes_the_little_endian_bytes() {
        let mut words = vec![0, 1, 0xff, 0x100, u64::MAX, 1 << 63, 0x00ab_0000_0000_00cd];
        words.extend([u64::MAX; 300]); // from many states, so every low byte is seen
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..1000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            words.push(x >> (x % 64)); // every byte length
        }
        let (mut fast, mut bytes) = (Fnv::default(), Fnv::default());
        for w in words {
            fast.write_u64(w);
            bytes.write(&w.to_le_bytes());
            assert_eq!(fast.finish(), bytes.finish(), "after {w:#x}");
        }
    }

    #[test]
    fn the_all_ones_table_holds_for_every_low_byte() {
        for l in 0..256u64 {
            let start = 0x0123_4567_89ab_cd00 | l;
            let (mut fast, mut bytes) = (Fnv(start), Fnv(start));
            fast.write_u64(u64::MAX);
            bytes.write(&[0xff; 8]);
            assert_eq!(fast.finish(), bytes.finish(), "low byte {l:#x}");
        }
    }

    /// Byte-at-a-time FNV-1a 64 of the records below, computed independently (Python).
    const STABLE_HASH: u64 = 0xc203_6e73_c41a_3c0b;

    /// The trace hash of a fixed record sequence, as recorded before `write_u64` and the
    /// fixed ring: replays and failure signatures compare such values across versions.
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
    fn every_fixed_tail_table_holds_for_every_low_byte() {
        for (kind, arg, tail) in [
            (TraceKind::Step, 0, &STEP_TAIL),
            (TraceKind::ReadOnly, 0, &READ_ONLY_TAIL),
            (TraceKind::Eval, 3, &EVAL_TAILS[3]),
        ] {
            let bytes = record_tail(kind as u8, arg);
            for l in 0..=255u8 {
                let start = 0x0fed_cba9_8765_4300 | u64::from(l);
                let mut plain = Fnv(start);
                plain.write(&bytes);
                let fixed = start
                    .wrapping_mul(PRIME_POW_32)
                    .wrapping_add(tail.table[usize::from(l)]);
                assert_eq!(fixed, plain.finish(), "{kind:?} {arg}, low byte {l:#x}");
            }
        }
    }

    /// A record with a fixed tail hashes to the same value through its table, from every
    /// starting low byte (the states before it vary with the preceding records).
    #[test]
    fn a_fixed_tail_hashes_as_its_bytes_do() {
        let tails = [
            (TraceKind::Step, 0, &STEP_TAIL),
            (TraceKind::ReadOnly, 0, &READ_ONLY_TAIL),
            (TraceKind::Eval, 0, &EVAL_TAILS[0]),
            (TraceKind::Eval, 1, &EVAL_TAILS[1]),
            (TraceKind::Eval, 2, &EVAL_TAILS[2]),
            (TraceKind::Eval, 3, &EVAL_TAILS[3]),
        ];
        for (kind, arg, tail) in tails {
            for before in 0..256u64 {
                let (plain, fixed) = (Recorder::default(), Recorder::default());
                for r in [&plain, &fixed] {
                    r.record(TraceRecord {
                        value: before * 0x9e37,
                        ..rec(before)
                    });
                }
                let record = TraceRecord {
                    time: SimTime(5000 * before + 17),
                    kind,
                    task: u64::MAX,
                    arg,
                    value: 0,
                };
                plain.record(record);
                fixed.record_fixed(record, tail);
                assert_eq!(fixed.hash(), plain.hash(), "{kind:?} {arg} after {before}");
                assert_eq!(fixed.recent(), plain.recent());
            }
        }
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
