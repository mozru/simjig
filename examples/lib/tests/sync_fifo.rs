//! `sync_fifo` (`rtl/sync_fifo.sv`) in both read modes: the 8-bit `sync_fifo` model has a
//! registered read, and `item_fifo` (the same FIFO over the packed struct `Item`) is
//! first-word-fall-through.
//!
//! The testbench's shape:
//! - **Timing.** Every task drives on the falling edge and samples on the rising edge. At a
//!   rising edge of a testbench clock, signals still hold their pre-edge values, which are
//!   exactly what the flops capture, so a monitor sees each push and pop the design takes.
//! - **Reference model.** [`Model`] is the FIFO in plain Rust. A monitor task checks it
//!   against the design on every edge (`count`, `full`, `empty`, `rd`), then applies the
//!   edge's push and pop to it, so every test, directed or random, is checked the same way.
//! - **Stimulus.** A producer and a consumer task draw from their own seeded streams
//!   (`ctx.rng("producer")`), so the consumer's backpressure and the producer's bursts are
//!   independent and replay exactly from the report's seed.
#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions: a panic is the test failure"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::rc::Rc;

use simjig::bits::{Bit, Bits, Packed};
use simjig::with_timeout;
use simjig::{In, Out, RawWords, ReadOnly, Rng, TestCtx, clock, cycles, fork, ns, spawn, us};

mod common;

simjig::include_dut!(sync_fifo);
simjig::include_dut!(item_fifo);

use item_fifo::{Item, Kind};

/// Cycles of random traffic per test.
const CYCLES: u32 = 2000;
/// The producer and consumer redraw their rate every `PHASE` cycles: long bursts fill and
/// drain the FIFO, which uniform coin flips at 50% would rarely do.
const PHASE: u32 = 24;
/// Rates (percent of cycles) a producer or consumer picks from for each phase.
const RATES: [u64; 3] = [10, 50, 90];

/// A value the generated handles can read and write (`In::set` and `Out::get` need
/// `RawWords` storage). Its `Default` is the all-zero value
/// (`Bits::ZERO`; every field zero for a generated struct): a registered `rd`'s reset
/// value.
trait Payload: Packed<Raw: RawWords> + PartialEq + Debug + Default {}
impl<T: Packed<Raw: RawWords> + PartialEq + Debug + Default> Payload for T {}

/// One FIFO's handles, whichever model they come from.
///
/// Why this struct exists: `build.rs` builds two models of `sync_fifo` (the raw 8-bit one
/// and `item_fifo`), and every test here should run the same driver, monitor and
/// [`Model`] on either. But each generated `Dut` is its own type, with no trait in common,
/// and their payloads differ (`Bits<8>` and `Item`). Copying the handles into one
/// `Fifo<T>` is the smallest code that lets the testbench be written once, generic only
/// in the payload; the alternative is a second copy of every helper per model.
#[derive(Clone, Copy, Debug)]
struct Fifo<T: 'static> {
    clk: In<Bit>,
    rst: In<Bit>,
    push: In<Bit>,
    wr: In<T>,
    pop: In<Bit>,
    rd: Out<T>,
    full: Out<Bit>,
    empty: Out<Bit>,
    count: Out<Bits<3>>,
    depth: usize,
    fwft: bool,
}

impl Fifo<Bits<8>> {
    fn raw(dut: sync_fifo::Dut) -> Self {
        Self {
            clk: dut.clk,
            rst: dut.rst,
            push: dut.push,
            wr: dut.wr_data,
            pop: dut.pop,
            rd: dut.rd_data,
            full: dut.full,
            empty: dut.empty,
            count: dut.count,
            depth: usize::try_from(sync_fifo::Dut::DEPTH).unwrap(),
            fwft: sync_fifo::Dut::FWFT,
        }
    }
}

impl Fifo<Item> {
    fn items(dut: item_fifo::Dut) -> Self {
        Self {
            clk: dut.clk,
            rst: dut.rst,
            push: dut.push,
            wr: dut.wr_item,
            pop: dut.pop,
            rd: dut.rd_item,
            full: dut.full,
            empty: dut.empty,
            count: dut.count,
            depth: usize::try_from(item_fifo::Dut::DEPTH).unwrap(),
            fwft: item_fifo::Dut::FWFT,
        }
    }
}

/// The ports at one instant.
#[derive(Clone, Copy, Debug)]
#[expect(clippy::struct_excessive_bools, reason = "one field per 1-bit port")]
struct Sample<T> {
    rst: bool,
    push: bool,
    wr: T,
    pop: bool,
    rd: T,
    full: bool,
    empty: bool,
    count: usize,
}

impl<T: Payload> Fifo<T> {
    fn sample(self) -> Sample<T> {
        Sample {
            rst: self.rst.get().to_bool(),
            push: self.push.get().to_bool(),
            wr: self.wr.get(),
            pop: self.pop.get().to_bool(),
            rd: self.rd.get(),
            full: self.full.get().to_bool(),
            empty: self.empty.get().to_bool(),
            count: usize::try_from(self.count.get().to_u128()).unwrap(),
        }
    }
}

/// What the monitor saw, for the tests' coverage assertions.
#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    pushes: u32,
    pops: u32,
    /// Pushes a full FIFO dropped (no pop in the same cycle).
    dropped: u32,
    /// Edges where the FIFO was full, and where it was empty.
    full: u32,
    empty: u32,
    /// Pushes taken together with a pop while full: the case that needs `|| do_pop`.
    push_pop_full: u32,
    /// Reset edges after the first reset, and the entries they discarded.
    resets: u32,
    flushed: u32,
}

/// The reference model: the FIFO's entries, and what `rd` must show.
#[derive(Debug)]
struct Model<T> {
    depth: usize,
    fwft: bool,
    entries: VecDeque<T>,
    /// Registered read only: what `rd` holds, the entry the last pop removed (its reset
    /// value, zero, before the first pop after a reset).
    shown: T,
    stats: Stats,
}

impl<T: Payload> Model<T> {
    /// A model of `fifo` holding `entries`, oldest first (none right after reset).
    fn new(fifo: Fifo<T>, entries: &[T]) -> Self {
        Self {
            depth: fifo.depth,
            fwft: fifo.fwft,
            entries: entries.iter().copied().collect(),
            shown: T::default(),
            stats: Stats::default(),
        }
    }

    /// Checks the outputs sampled just before an edge, then applies that edge: a reset,
    /// which discards every entry whatever `push` and `pop` say, or the push and pop.
    fn step(&mut self, s: &Sample<T>) {
        let len = self.entries.len();
        assert_eq!(s.count, len, "count");
        assert_eq!(s.full, len == self.depth, "full, with {len} entries");
        assert_eq!(s.empty, len == 0, "empty, with {len} entries");
        let want = if self.fwft {
            self.entries.front()
        } else {
            Some(&self.shown)
        };
        if let Some(want) = want {
            assert_eq!(s.rd, *want, "rd");
        }
        if s.rst {
            self.stats.resets += 1;
            self.stats.flushed += u32::try_from(len).unwrap();
            self.entries.clear();
            self.shown = T::default();
            return;
        }
        let pop = s.pop && len > 0;
        let push = s.push && (len < self.depth || pop);
        if pop {
            let head = self.entries.pop_front().unwrap();
            if !self.fwft {
                self.shown = head;
            }
        }
        if push {
            self.entries.push_back(s.wr);
        }
        let st = &mut self.stats;
        st.pops += u32::from(pop);
        st.pushes += u32::from(push);
        st.dropped += u32::from(s.push && !push);
        st.full += u32::from(len == self.depth);
        st.empty += u32::from(len == 0);
        st.push_pop_full += u32::from(push && pop && len == self.depth);
    }
}

/// Starts a monitor that checks `model` against `fifo` on every rising edge until the
/// test ends, and returns the model for the test to inspect.
fn monitor<T: Payload>(fifo: Fifo<T>, model: Model<T>) -> Rc<RefCell<Model<T>>> {
    let board = Rc::new(RefCell::new(model));
    let shared = Rc::clone(&board);
    spawn("monitor", async move {
        loop {
            fifo.clk.rising().await;
            let s = fifo.sample();
            shared.borrow_mut().step(&s);
        }
    })
    .detach();
    board
}

/// Holds reset for two cycles with every input idle, and releases it at the falling edge
/// after the last reset edge, where it returns: the next edge is the first out of reset.
async fn reset<T: Payload>(fifo: Fifo<T>) {
    fifo.rst.set(1);
    fifo.push.set(0);
    fifo.pop.set(0);
    cycles(fifo.clk, 2).await;
    fifo.clk.falling().await;
    fifo.rst.set(0);
}

/// Starts the clock, resets, and starts the monitor on an empty model.
async fn start<T: Payload>(fifo: Fifo<T>) -> Rc<RefCell<Model<T>>> {
    clock(fifo.clk, ns(10)).start();
    reset(fifo).await;
    monitor(fifo, Model::new(fifo, &[]))
}

/// One directed cycle: sets the inputs at the falling edge, and returns the outputs after
/// the next rising edge (in `ReadOnly`, once they settled).
async fn cycle<T: Payload>(fifo: Fifo<T>, push: Option<T>, pop: bool) -> Sample<T> {
    fifo.clk.falling().await;
    fifo.push.set(push.is_some());
    if let Some(item) = push {
        fifo.wr.set(item);
    }
    fifo.pop.set(pop);
    fifo.clk.rising().await;
    ReadOnly::new().await;
    fifo.sample()
}

fn pick(rng: &mut Rng, rates: &[u64]) -> u64 {
    rates[usize::try_from(rng.below(rates.len() as u64)).unwrap()]
}

/// Pushes random items for `cycles` cycles, in bursts (see [`PHASE`]), then goes idle.
async fn produce<T: Payload>(fifo: Fifo<T>, mut rng: Rng, cycles: u32, item: fn(&mut Rng) -> T) {
    let mut rate = 0;
    for n in 0..cycles {
        if n % PHASE == 0 {
            rate = pick(&mut rng, &RATES);
        }
        fifo.clk.falling().await;
        fifo.push.set(rng.below(100) < rate);
        fifo.wr.set(item(&mut rng));
    }
    fifo.clk.falling().await;
    fifo.push.set(0);
}

/// Pops at a random rate for `cycles` cycles (the backpressure), then stops popping.
async fn consume<T: Payload>(fifo: Fifo<T>, mut rng: Rng, cycles: u32) {
    let mut rate = 0;
    for n in 0..cycles {
        if n % PHASE == 0 {
            rate = pick(&mut rng, &RATES);
        }
        fifo.clk.falling().await;
        fifo.pop.set(rng.below(100) < rate);
    }
    fifo.clk.falling().await;
    fifo.pop.set(0);
}

/// Pops until the FIFO is empty, then lets the monitor check one more (empty) edge.
async fn drain<T: Payload>(fifo: Fifo<T>) {
    fifo.clk.falling().await;
    fifo.pop.set(1);
    while fifo.empty.get() == Bit::ZERO {
        fifo.clk.falling().await;
    }
    fifo.pop.set(0);
    fifo.clk.rising().await;
    ReadOnly::new().await;
}

/// Producer and consumer for `cycles` cycles, then a drain (at most 1 us).
async fn traffic<T: Payload>(fifo: Fifo<T>, rng: [Rng; 2], cycles: u32, item: fn(&mut Rng) -> T) {
    let [push_rng, pop_rng] = rng;
    let producer = spawn("producer", produce(fifo, push_rng, cycles, item));
    let consumer = spawn("consumer", consume(fifo, pop_rng, cycles));
    producer.await;
    consumer.await;
    with_timeout(us(1), drain(fifo))
        .await
        .expect("the FIFO drains within 1 us once the producer stops");
}

/// Random traffic, then a drain: every pushed entry comes out once, in order (the
/// monitor checks each against the model), and the traffic reached every corner.
async fn random_traffic<T: Payload>(ctx: &TestCtx, fifo: Fifo<T>, item: fn(&mut Rng) -> T) {
    let board = start(fifo).await;
    let rng = [ctx.rng("producer"), ctx.rng("consumer")];
    traffic(fifo, rng, CYCLES, item).await;
    let model = board.borrow();
    let st = model.stats;
    assert!(model.entries.is_empty());
    assert_eq!(st.pops, st.pushes, "every entry pushed was popped once");
    assert!(st.full > 0 && st.empty > 0, "{st:?}");
    assert!(st.dropped > 0 && st.push_pop_full > 0, "{st:?}");
}

/// Resets the FIFO in the middle of random traffic, from a state Verilator's all-zero
/// start never shows: entries inside and both pointers away from zero. Right after the
/// reset edge the FIFO is empty (`count` 0, `rd` zero in registered mode); then traffic
/// goes on against the model, which the reset restarted empty.
async fn reset_during_traffic<T: Payload>(ctx: &TestCtx, fifo: Fifo<T>, item: fn(&mut Rng) -> T) {
    let board = start(fifo).await;
    let resetter = spawn("resetter", reset_when_busy(fifo, Rc::clone(&board)));
    let rng = [ctx.rng("producer"), ctx.rng("consumer")];
    traffic(fifo, rng, CYCLES, item).await;
    let after = resetter.await;
    assert_eq!(
        (after.count, after.empty, after.full),
        (0, true, false),
        "{after:?}"
    );
    if !fifo.fwft {
        assert_eq!(after.rd, T::default(), "rd after reset");
    }
    let model = board.borrow();
    let st = model.stats;
    assert_eq!(st.resets, 1);
    assert!(st.flushed > 0, "{st:?}");
    assert_eq!(st.pops + st.flushed, st.pushes, "{st:?}");
    assert!(st.full > 0 && st.empty > 0, "{st:?}");
}

/// Waits, checking at each falling edge, until the FIFO holds two entries or more and
/// neither pointer is at zero; then holds `rst` for one edge while the producer and
/// consumer keep driving `push` and `pop`. Returns the outputs right after that edge.
async fn reset_when_busy<T: Payload>(fifo: Fifo<T>, board: Rc<RefCell<Model<T>>>) -> Sample<T> {
    // The pointers count modulo 2 * DEPTH (one wrap bit above the index).
    let wrap = u32::try_from(2 * fifo.depth).unwrap();
    let busy = async {
        loop {
            fifo.clk.falling().await;
            let model = board.borrow();
            let st = model.stats;
            if model.entries.len() >= 2
                && !st.pops.is_multiple_of(wrap)
                && !st.pushes.is_multiple_of(wrap)
            {
                break;
            }
        }
    };
    with_timeout(us(5), busy)
        .await
        .expect("the FIFO holds entries with both pointers moved within 5 us");
    fifo.rst.set(1);
    fifo.clk.rising().await;
    ReadOnly::new().await;
    let after = fifo.sample();
    fifo.clk.falling().await;
    fifo.rst.set(0);
    after
}

/// Directed: a reset beats a push and a pop at the same edge. Three pushes and a pop
/// leave two entries with both pointers away from zero; then `push` and `pop` are held
/// high across the reset edge. After it the FIFO is empty (`count` 0, `empty` set,
/// `full` clear) and a registered `rd` is zero, not the entry the pop would have read;
/// one push and pop then show it working again. Random traffic reaches this edge only on
/// a lucky seed.
async fn reset_beats_push_and_pop<T: Payload>(fifo: Fifo<T>, entries: [T; 4]) {
    let board = start(fifo).await;
    let [a, b, c, d] = entries;
    for entry in [a, b, c] {
        cycle(fifo, Some(entry), false).await;
    }
    let before = cycle(fifo, None, true).await;
    assert_eq!(before.count, 2, "two entries before the reset");
    fifo.clk.falling().await;
    fifo.rst.set(1);
    fifo.push.set(1);
    fifo.wr.set(d);
    fifo.pop.set(1);
    fifo.clk.rising().await;
    ReadOnly::new().await;
    let after = fifo.sample();
    assert_eq!(
        (after.count, after.empty, after.full),
        (0, true, false),
        "reset beats push and pop: {after:?}"
    );
    if !fifo.fwft {
        assert_eq!(
            after.rd,
            T::default(),
            "rd after reset, not the popped {b:?}"
        );
    }
    fifo.clk.falling().await;
    fifo.rst.set(0);
    fifo.push.set(0);
    fifo.pop.set(0);
    cycle(fifo, Some(d), false).await;
    let last = cycle(fifo, None, true).await;
    assert!(last.empty, "the push after the reset was popped: {last:?}");
    if !fifo.fwft {
        assert_eq!(last.rd, d, "the only entry after the reset");
    }
    cycle(fifo, None, false).await; // the monitor checks the edge after the pop
    let st = board.borrow().stats;
    assert_eq!((st.resets, st.flushed), (1, 2), "{st:?}");
}

#[simjig::test(init = random, timeout = "2us")]
async fn a_reset_beats_push_and_pop(dut: sync_fifo::Dut) {
    let entries = [0x11, 0x22, 0x33, 0x44].map(Bits::new);
    reset_beats_push_and_pop(Fifo::raw(dut), entries).await;
}

#[simjig::test(init = random, timeout = "2us")]
async fn a_reset_beats_item_push_and_pop(dut: item_fifo::Dut) {
    let entries = [1, 2, 3, 4].map(|i| item(Kind::DATA, i, 0x1111 * i));
    reset_beats_push_and_pop(Fifo::items(dut), entries).await;
}

#[simjig::test(init = random, timeout = "100us")]
async fn a_reset_during_traffic_empties_the_fifo(ctx: &TestCtx, dut: sync_fifo::Dut) {
    reset_during_traffic(ctx, Fifo::raw(dut), random_bits).await;
}

#[simjig::test(init = random, timeout = "100us")]
async fn a_reset_during_item_traffic_empties_the_fifo(ctx: &TestCtx, dut: item_fifo::Dut) {
    reset_during_traffic(ctx, Fifo::items(dut), random_item).await;
}

fn random_bits(rng: &mut Rng) -> Bits<8> {
    Bits::new(u128::from(rng.below(1 << 8)))
}

fn random_item(rng: &mut Rng) -> Item {
    Item {
        kind: Kind::from_bits(Bits::new(u128::from(rng.below(4)))),
        tag: Bits::new(u128::from(rng.below(1 << 6))),
        data: Bits::new(u128::from(rng.below(1 << 16))),
    }
}

fn item(kind: Kind, tag: u128, data: u128) -> Item {
    Item {
        kind,
        tag: Bits::new(tag),
        data: Bits::new(data),
    }
}

#[simjig::test(init = random, timeout = "100us")]
async fn random_traffic_matches_the_model(ctx: &TestCtx, dut: sync_fifo::Dut) {
    random_traffic(ctx, Fifo::raw(dut), random_bits).await;
}

#[simjig::test(init = random, timeout = "100us")]
async fn random_items_match_the_model(ctx: &TestCtx, dut: item_fifo::Dut) {
    random_traffic(ctx, Fifo::items(dut), random_item).await;
}

/// Five pushes into four entries: `count` climbs to `full` and the fifth push is dropped;
/// four pops return the first four in order, and a pop of an empty FIFO is ignored.
#[simjig::test(init = random, timeout = "2us")]
async fn fills_to_full_and_drains_to_empty(dut: sync_fifo::Dut) {
    let fifo = Fifo::raw(dut);
    let board = start(fifo).await;
    let items: Vec<Bits<8>> = (1..=5).map(|i| Bits::new(0x11 * i)).collect();
    for (n, &item) in items.iter().enumerate() {
        let s = cycle(fifo, Some(item), false).await;
        assert_eq!(s.count, (n + 1).min(fifo.depth));
        assert_eq!(s.full, n + 1 >= fifo.depth);
    }
    for (n, &item) in items[..fifo.depth].iter().enumerate() {
        let s = cycle(fifo, None, true).await;
        // Registered read: the entry a pop removes is on `rd` after the pop's edge.
        assert_eq!(s.rd, item, "pop {n}");
        assert!(!s.full);
    }
    let s = cycle(fifo, None, true).await;
    assert!(s.empty);
    assert_eq!(s.count, 0);
    assert_eq!(
        s.rd,
        items[fifo.depth - 1],
        "a pop of an empty FIFO leaves rd as it was"
    );
    cycle(fifo, None, false).await;
    assert_eq!(board.borrow().stats.dropped, 1);
}

/// A push and a pop in one cycle: when empty only the push is taken; in between `count`
/// holds; when full both are taken, so the FIFO stays full with the new entry last.
#[simjig::test(init = random, timeout = "2us")]
async fn push_and_pop_in_one_cycle(dut: sync_fifo::Dut) {
    let fifo = Fifo::raw(dut);
    let board = start(fifo).await;
    let b = |v: u128| Bits::<8>::new(v);
    let s = cycle(fifo, Some(b(0xa0)), true).await;
    assert_eq!(s.count, 1, "empty: the pop is ignored, the push taken");
    let s = cycle(fifo, Some(b(0xa1)), true).await;
    assert_eq!((s.count, s.rd), (1, b(0xa0)), "one in, one out");
    for v in 0xa2..=0xa4 {
        cycle(fifo, Some(b(v)), false).await;
    }
    let s = cycle(fifo, Some(b(0xa5)), true).await;
    assert!(s.full, "full: a push with a pop is taken");
    assert_eq!(s.rd, b(0xa1));
    let mut out = Vec::new();
    for _ in 0..fifo.depth {
        out.push(cycle(fifo, None, true).await.rd.to_u128());
    }
    assert_eq!(out, [0xa2, 0xa3, 0xa4, 0xa5]);
    assert_eq!(board.borrow().stats.push_pop_full, 1);
}

/// Registered read: a pushed entry stays off `rd` (which holds its reset value) until a
/// pop, and appears on the edge of that pop.
#[simjig::test(init = random, timeout = "2us")]
async fn registered_read_waits_for_a_pop(dut: sync_fifo::Dut) {
    let fifo = Fifo::raw(dut);
    start(fifo).await;
    cycle(fifo, Some(Bits::new(0x5a)), false).await;
    for _ in 0..3 {
        let s = cycle(fifo, None, false).await;
        assert_eq!((s.empty, s.rd), (false, Bits::new(0)));
    }
    let s = cycle(fifo, None, true).await;
    assert_eq!((s.empty, s.rd), (true, Bits::new(0x5a)));
}

/// First-word fall-through: the head is on `rd` the edge after its push, without a pop,
/// and each pop puts the next entry there on its own edge.
#[simjig::test(init = random, timeout = "2us")]
async fn first_word_falls_through(dut: item_fifo::Dut) {
    let fifo = Fifo::items(dut);
    start(fifo).await;
    let a = item(Kind::DATA, 1, 0xaaaa);
    let b = item(Kind::LAST, 2, 0xbbbb);
    let s = cycle(fifo, Some(a), false).await;
    assert_eq!((s.empty, s.rd), (false, a), "the head shows with no pop");
    let s = cycle(fifo, Some(b), false).await;
    assert_eq!(s.rd, a, "a later push leaves the head");
    let s = cycle(fifo, None, true).await;
    assert_eq!(s.rd, b, "the pop's edge brings the next entry");
    assert_eq!(s.rd.kind, Kind::LAST);
    let s = cycle(fifo, None, true).await;
    assert!(s.empty);
}

/// Fills the FIFO, then forks four branches of random traffic from that full state, each
/// with its own seed: the corner every branch starts from is reached once. Each branch
/// gets its own monitor (a checkpoint needs every other task finished), seeded with the
/// entries the fill left.
#[simjig::test(init = random, timeout = "100us")]
async fn random_continuations_from_full(ctx: &TestCtx, dut: item_fifo::Dut) {
    let fifo = Fifo::items(dut);
    clock(fifo.clk, ns(10)).start();
    reset(fifo).await;
    let fill: Vec<Item> = (0..fifo.depth as u128)
        .map(|i| item(Kind::CTRL, i, i))
        .collect();
    for &entry in &fill {
        cycle(fifo, Some(entry), false).await;
    }
    assert!(fifo.full.get().to_bool());
    let forked = fork::run_from_checkpoint(ctx.seed(), 4, |branch| {
        let model = Model::new(fifo, &fill);
        async move {
            let board = monitor(fifo, model);
            let rng = [branch.rng("producer"), branch.rng("consumer")];
            traffic(fifo, rng, 300, random_item).await;
            let st = board.borrow().stats;
            Ok((st.pushes, st.pops))
        }
    })
    .await;
    let branches = match forked {
        Ok(branches) => branches,
        // Multi-threaded `cargo test`: a fork is safe only with one test per process.
        Err(e @ fork::ForkError::Threads { .. }) => {
            eprintln!("skipped: {e}");
            return;
        }
        Err(e) => panic!("the fill ends at a quiet point: {e}"),
    };
    let counts = branches.into_values();
    let filled = u32::try_from(fill.len()).unwrap();
    for &(pushes, pops) in &counts {
        assert_eq!(
            pops,
            pushes + filled,
            "the fill and every push come out: {counts:?}"
        );
    }
    assert!(
        counts.windows(2).any(|w| w[0] != w[1]),
        "branches differ: {counts:?}"
    );
}

/// The elaboration checks: Verilator lints every supported shape cleanly with `-Wall`,
/// and stops on a DEPTH that is not a power of two or a WIDTH below 1.
#[test]
fn parameters_are_checked_at_elaboration() {
    let src = &["rtl/sync_fifo.sv"];
    let good: &[&[&str]] = &[&["DEPTH=2"], &["DEPTH=16", "WIDTH=33", "FWFT=1"]];
    let bad_depth: &[&[&str]] = &[&["DEPTH=1"], &["DEPTH=6"]];
    common::assert_param_checks(
        "sync_fifo",
        src,
        good,
        bad_depth,
        "DEPTH must be a power of two",
    );
    let bad_width: &[&[&str]] = &[&["WIDTH=0"]];
    common::assert_param_checks("sync_fifo", src, &[], bad_width, "WIDTH must be at least 1");
}
