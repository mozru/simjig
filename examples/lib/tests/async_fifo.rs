//! Testbench for `rtl/async_fifo.sv`, built with `WIDTH = 8`, `DEPTH = 8`.
//!
//! Two kernel clocks with unrelated periods drive the two domains. The two resets always
//! fall together, as the RTL requires, and each rises on its own clock. Each driver
//! drives at the falling edge of its own domain's clock, and each monitor samples at the
//! rising edge. The testbench is split by
//! role, one task each:
//! - the **writer** drives `winc`/`wdata` on `wclk`, stalling at random; it keeps `winc`
//!   high while `wfull` is, so the FIFO must refuse those writes;
//! - the **reader** drives `rinc` on `rclk`, stalling at random, also regardless of
//!   `rempty`;
//! - the **write monitor** sees each write the FIFO accepts (`winc && !wfull` at a `wclk`
//!   edge) and puts its data into the scoreboard;
//! - the **read monitor** sees each read the FIFO performs (`rinc && !rempty` at an `rclk`
//!   edge) and checks `rdata` against the scoreboard's oldest entry.
//!
//! The scoreboard is a [`Queue`] with room for exactly `DEPTH` entries: the writes accepted
//! and not yet read. A write accepted while the FIFO holds `DEPTH` entries overflows it
//! (`wfull` was optimistic); a read with the scoreboard empty means `rempty` was. Order and
//! data are checked per read, and the test ends only when every written item was read. A
//! reset empties the scoreboard (the FIFO discards what it held), so a test may reset
//! mid-run.
//!
//! These are 2-state, zero-delay simulations: a synchronizer flop here never goes
//! metastable, and a pointer crossing is never sampled mid-change. They check the
//! protocol (order, loss, flag latency, reset); the CDC structure itself (Gray pointers,
//! two-flop synchronizers, nothing else crossing) is verified by review and CDC lint.
//!
//! Monitors sample at `rising()`, which for a testbench clock shows the pre-edge values:
//! the `winc`, `wfull` and `rdata` the edge acts on.
#![forbid(unsafe_code)]

use std::cell::Cell;
use std::rc::Rc;

use simjig::bits::{Bit, Bits};
use simjig::{
    In, Out, Queue, ReadOnly, Rng, SimDuration, TestCtx, Timer, clock, cycles, join, ns, ps, spawn,
    us, with_timeout,
};

mod common;

simjig::include_dut!(async_fifo);

use async_fifo::Dut;

/// The FIFO's depth, from the build script (`.param("DEPTH", 8)`).
const DEPTH: usize = Dut::DEPTH as usize;

/// One FIFO entry.
type Word = Bits<{ Dut::WIDTH }>;

/// Items each traffic test writes and reads back.
const ITEMS: u32 = 1000;

/// The two clocks: periods and the read clock's first rising edge.
#[derive(Clone, Copy, Debug)]
struct Clocks {
    wclk: SimDuration,
    rclk: SimDuration,
    /// Offsets `rclk` from `wclk`, so their edges are not aligned at time 0.
    rclk_phase: SimDuration,
}

fn start_clocks(dut: Dut, clocks: Clocks) {
    clock(dut.wclk, clocks.wclk).start();
    clock(dut.rclk, clocks.rclk)
        .phase(clocks.rclk_phase)
        .start();
}

/// Idles both sides and resets the FIFO ([`reset_both`]).
async fn reset(dut: Dut) {
    dut.winc.set(0);
    dut.wdata.set(0);
    dut.rinc.set(0);
    reset_both(dut).await;
}

/// Asserts both resets at once, now, and releases each after three cycles of its own clock,
/// just after a falling edge of that clock (away from the rising edge its flops use).
/// Both must fall together: one reset alone leaves the two pointers inconsistent (see
/// `rtl/async_fifo.sv`).
async fn reset_both(dut: Dut) {
    dut.wrst_n.set(0);
    dut.rrst_n.set(0);
    join!(release(dut.wclk, dut.wrst_n), release(dut.rclk, dut.rrst_n));
}

async fn release(clk: In<Bit>, rst_n: In<Bit>) {
    cycles(clk, 3).await;
    clk.falling().await;
    rst_n.set(1);
}

/// A random stall pattern: runs of busy and slow periods, so the FIFO both fills up and
/// runs dry whatever the clock ratio.
#[derive(Debug)]
struct Stalls {
    rng: Rng,
    /// Out of 8: the chance of being active in a cycle of the current run.
    level: u64,
    left: u64,
}

impl Stalls {
    fn new(rng: Rng) -> Self {
        Self {
            rng,
            level: 4,
            left: 0,
        }
    }

    /// Whether to be active this cycle.
    fn active(&mut self) -> bool {
        if self.left == 0 {
            self.level = 1 + self.rng.below(7);
            self.left = 16 + self.rng.below(48);
        }
        self.left -= 1;
        self.rng.below(8) < self.level
    }
}

/// The writer: drives `ITEMS` random words, each held on `wdata` with `winc` high until
/// the FIFO takes it, with random idle cycles in between.
async fn writer(dut: Dut, mut data: Rng, mut stalls: Stalls) {
    for _ in 0..ITEMS {
        while !stalls.active() {
            dut.wclk.falling().await;
            dut.winc.set(0);
        }
        dut.wclk.falling().await;
        dut.winc.set(1);
        dut.wdata.set(Word::new_truncating(data.next_u64().into()));
        dut.wclk.rising().await;
        // Pre-edge `wfull`: whether this edge took the word.
        while dut.wfull.get().to_bool() {
            dut.wclk.rising().await;
        }
    }
    dut.wclk.falling().await;
    dut.winc.set(0);
}

/// The reader: drives `rinc` with random stalls, forever (it is detached).
async fn reader(dut: Dut, mut stalls: Stalls) {
    loop {
        dut.rclk.falling().await;
        dut.rinc.set(stalls.active());
    }
}

/// What the monitors counted, to show the test reached both boundaries.
#[derive(Debug, Default)]
struct Coverage {
    /// `wclk` edges with `winc` high that `wfull` refused.
    writes_refused: Cell<u32>,
    /// `rclk` edges with `rinc` high that `rempty` refused.
    reads_refused: Cell<u32>,
    /// Writes the FIFO accepted, reads it performed, and entries a reset discarded.
    writes: Cell<u32>,
    reads: Cell<u32>,
    flushed: Cell<u32>,
}

fn bump(counter: &Cell<u32>) {
    counter.set(counter.get() + 1);
}

/// Records every write the FIFO accepts into the scoreboard.
async fn write_monitor(dut: Dut, scoreboard: Queue<Word>, coverage: Rc<Coverage>) {
    loop {
        dut.wclk.rising().await;
        if !dut.wrst_n.get().to_bool() || !dut.winc.get().to_bool() {
            continue;
        }
        if dut.wfull.get().to_bool() {
            bump(&coverage.writes_refused);
            continue;
        }
        let word = dut.wdata.get();
        assert!(
            scoreboard.try_put(word).is_ok(),
            "a write of {word:?} was accepted with {DEPTH} entries unread: wfull was low on a \
             full FIFO"
        );
        bump(&coverage.writes);
    }
}

/// Checks every read the FIFO performs against the scoreboard, in order.
async fn read_monitor(dut: Dut, scoreboard: Queue<Word>, coverage: Rc<Coverage>) {
    loop {
        dut.rclk.rising().await;
        if !dut.rrst_n.get().to_bool() || !dut.rinc.get().to_bool() {
            continue;
        }
        if dut.rempty.get().to_bool() {
            bump(&coverage.reads_refused);
            continue;
        }
        let Some(expected) = scoreboard.try_get() else {
            panic!("a read with nothing written: rempty was low on an empty FIFO");
        };
        let read = coverage.reads.get();
        assert_eq!(
            dut.rdata.get(),
            expected,
            "read #{read} out of order or corrupted"
        );
        bump(&coverage.reads);
    }
}

/// Runs `ITEMS` words through the FIFO under random stalls on both sides and checks
/// order, loss and both flags; with `mid_run_reset`, [`reset_when_busy`] resets the FIFO
/// once on the way. Returns what the monitors counted.
async fn traffic(ctx: &TestCtx, dut: Dut, clocks: Clocks, mid_run_reset: bool) -> Rc<Coverage> {
    start_clocks(dut, clocks);
    reset(dut).await;
    let scoreboard = Queue::new("scoreboard", DEPTH);
    let coverage = Rc::new(Coverage::default());
    let monitor = write_monitor(dut, scoreboard.clone(), Rc::clone(&coverage));
    spawn("write monitor", monitor).detach();
    let monitor = read_monitor(dut, scoreboard.clone(), Rc::clone(&coverage));
    spawn("read monitor", monitor).detach();
    let resetter = mid_run_reset.then(|| {
        let busy = reset_when_busy(dut, scoreboard.clone(), Rc::clone(&coverage));
        spawn("resetter", busy)
    });
    spawn("reader", reader(dut, Stalls::new(ctx.rng("read stalls")))).detach();
    let writer = spawn(
        "writer",
        writer(dut, ctx.rng("data"), Stalls::new(ctx.rng("write stalls"))),
    );
    // The writer ends at a falling edge, after the write monitor saw its last write.
    let all_read = async {
        writer.await;
        while !scoreboard.is_empty() {
            dut.rclk.rising().await;
        }
    };
    // Far more than the slowest ratio needs: a lost item fails here, not at the test's limit.
    if let Err(elapsed) = with_timeout(us(400), all_read).await {
        let read = coverage.reads.get();
        panic!("{read} of {ITEMS} items read back: {elapsed}");
    }
    if let Some(resetter) = resetter {
        assert!(resetter.is_finished(), "the mid-run reset never came");
    } else {
        assert_eq!(coverage.writes.get(), ITEMS, "every item was accepted once");
    }
    let c = &coverage;
    assert_eq!(c.reads.get() + c.flushed.get(), c.writes.get(), "{c:?}");
    coverage
}

/// Waits, checking at each falling edge of `wclk`, until the FIFO holds two entries or
/// more, the read pointer is away from zero and the write pointer at least four writes
/// past it (so the read side's copy, which lags it by two `rclk` edges, is away from zero
/// too). Then resets both sides together while the writer and reader keep going, and
/// checks that both sides are empty at once, and stay so for the two edges of each clock
/// after its release: no write can reach `rempty` sooner (two synchronizer flops, then the
/// flag's). Empties the scoreboard, as the reset empties the FIFO.
async fn reset_when_busy(dut: Dut, scoreboard: Queue<Word>, coverage: Rc<Coverage>) {
    // The pointers count modulo 2 * DEPTH (one wrap bit above the address).
    let wrap = 2 * Dut::DEPTH;
    let busy = async {
        loop {
            dut.wclk.falling().await;
            let (writes, reads) = (coverage.writes.get() % wrap, coverage.reads.get() % wrap);
            if scoreboard.len() >= 2 && reads != 0 && writes >= 4 {
                break;
            }
        }
    };
    assert!(
        with_timeout(us(20), busy).await.is_ok(),
        "the FIFO never held entries with both pointers away from zero"
    );
    dut.wrst_n.set(0);
    dut.rrst_n.set(0);
    while scoreboard.try_get().is_some() {
        bump(&coverage.flushed);
    }
    ReadOnly::new().await;
    assert_eq!(
        (dut.rempty.get(), dut.wfull.get()),
        (Bit::ONE, Bit::ZERO),
        "in reset"
    );
    join!(
        async {
            release(dut.wclk, dut.wrst_n).await;
            assert_flag_holds(dut.wclk, dut.wfull, Bit::ZERO).await;
        },
        async {
            release(dut.rclk, dut.rrst_n).await;
            assert_flag_holds(dut.rclk, dut.rempty, Bit::ONE).await;
        }
    );
}

/// Asserts that `flag` reads `value` after each of the next two rising edges of `clk`.
async fn assert_flag_holds(clk: In<Bit>, flag: Out<Bit>, value: Bit) {
    for edge in 1..=2 {
        clk.rising().await;
        ReadOnly::new().await;
        assert_eq!(
            flag.get(),
            value,
            "`{}` at edge {edge} after reset",
            flag.name()
        );
    }
}

/// Fails unless the run met a full FIFO.
#[track_caller]
fn assert_filled_up(coverage: &Coverage) {
    assert!(
        coverage.writes_refused.get() > 0,
        "the FIFO never filled up"
    );
}

/// Fails unless the run met an empty FIFO.
#[track_caller]
fn assert_ran_dry(coverage: &Coverage) {
    assert!(coverage.reads_refused.get() > 0, "the FIFO never ran dry");
}

/// A write clock somewhat faster than the read clock.
#[simjig::test(timeout = "1ms")]
async fn traffic_with_a_7ns_writer_and_an_11ns_reader(ctx: &TestCtx, dut: Dut) {
    let clocks = Clocks {
        wclk: ns(7),
        rclk: ns(11),
        rclk_phase: ps(1300),
    };
    let coverage = traffic(ctx, dut, clocks, false).await;
    assert_filled_up(&coverage);
    assert_ran_dry(&coverage);
}

/// The same pair swapped: the reader is the faster side.
#[simjig::test(timeout = "1ms")]
async fn traffic_with_an_11ns_writer_and_a_7ns_reader(ctx: &TestCtx, dut: Dut) {
    let clocks = Clocks {
        wclk: ns(11),
        rclk: ns(7),
        rclk_phase: ps(2900),
    };
    let coverage = traffic(ctx, dut, clocks, false).await;
    assert_filled_up(&coverage);
    assert_ran_dry(&coverage);
}

/// A writer almost ten times faster than the reader, whose pointer moves several write
/// cycles apart: the FIFO is full most of the time.
#[simjig::test(timeout = "1ms")]
async fn traffic_with_a_fast_writer_and_a_slow_reader(ctx: &TestCtx, dut: Dut) {
    let clocks = Clocks {
        wclk: ps(3100),
        rclk: ns(29),
        rclk_phase: ps(700),
    };
    let coverage = traffic(ctx, dut, clocks, false).await;
    assert_filled_up(&coverage);
}

/// The reverse: a slow writer, so the FIFO is empty most of the time (and, the reader
/// being active at least one cycle in eight, never full).
#[simjig::test(timeout = "1ms")]
async fn traffic_with_a_slow_writer_and_a_fast_reader(ctx: &TestCtx, dut: Dut) {
    let clocks = Clocks {
        wclk: ns(29),
        rclk: ps(3100),
        rclk_phase: ps(700),
    };
    let coverage = traffic(ctx, dut, clocks, false).await;
    assert_ran_dry(&coverage);
}

/// Both resets asserted together in the middle of traffic, with entries inside and both
/// pointers away from zero: both sides read empty at once and through the first edges
/// after release, and traffic then runs on, every write after the reset read back once.
/// Verilator starts every flop at zero, so only a reset from such a state shows that each
/// reset clears its pointer and its synchronizer.
#[simjig::test(timeout = "1ms")]
async fn both_resets_mid_traffic_empty_both_sides(ctx: &TestCtx, dut: Dut) {
    let coverage = traffic(ctx, dut, directed_clocks(), true).await;
    assert!(coverage.flushed.get() >= 2, "{coverage:?}");
    assert!(coverage.reads.get() > 0, "{coverage:?}");
}

/// Clocks for the directed tests: no `rclk` edge ever coincides with a `wclk` edge (a
/// whole number of ns never equals one plus 0.3 ns), so each crossing takes a fixed number
/// of edges. A function, not a `const`: `ns()` converts to ticks of the running
/// simulation's precision.
fn directed_clocks() -> Clocks {
    Clocks {
        wclk: ns(7),
        rclk: ns(11),
        rclk_phase: ps(1300),
    }
}

/// Directed: each reset clears its side asynchronously, with no clock edge. Two words
/// written and read move the read pointer off zero; `DEPTH` more fill the FIFO (`wfull`
/// high) and reach the read side (`rempty` low). Then both resets fall together 1 ns
/// after a falling edge of `wclk`, away from every edge of both clocks: in `ReadOnly` of
/// that same instant, `wfull` is low and `rempty` high. Both are released and the FIFO
/// works again: three words written are read back in order. Random traffic resets at a
/// falling edge with the flags as they happen to be, which shows neither.
#[simjig::test(timeout = "10us")]
async fn resets_clear_both_flags_without_a_clock_edge(dut: Dut) {
    start_clocks(dut, directed_clocks());
    reset(dut).await;
    write_words(dut, 0..2).await;
    edges_until_low(dut.rclk, dut.rempty).await;
    read_words(dut, 0..2).await;
    write_words(dut, 2..2 + DEPTH).await;
    edges_until_low(dut.rclk, dut.rempty).await; // returns in `ReadOnly`
    assert_eq!(
        (dut.wfull.get(), dut.rempty.get()),
        (Bit::ONE, Bit::ZERO),
        "full"
    );
    // `wclk` edges fall on whole or half ns, `rclk` edges 0.3 ns off them (see
    // [`directed_clocks`]); the next `wclk` edge is 2.5 ns away.
    dut.wclk.falling().await;
    Timer::new(ns(1)).await;
    dut.wrst_n.set(0);
    dut.rrst_n.set(0);
    ReadOnly::new().await;
    assert_eq!(
        (dut.wfull.get(), dut.rempty.get()),
        (Bit::ZERO, Bit::ONE),
        "both flags as soon as both resets fall, before any clock edge"
    );
    join!(release(dut.wclk, dut.wrst_n), release(dut.rclk, dut.rrst_n));
    write_words(dut, 100..103).await;
    edges_until_low(dut.rclk, dut.rempty).await;
    read_words(dut, 100..103).await;
}

/// Writes the words `values`, one per `wclk` cycle, each at a falling edge; each must be
/// taken (`wfull` low at its edge).
async fn write_words(dut: Dut, values: std::ops::Range<usize>) {
    for v in values {
        dut.wclk.falling().await;
        dut.winc.set(1);
        dut.wdata.set(Word::new_truncating(v as u128));
        dut.wclk.rising().await;
        assert_eq!(dut.wfull.get(), Bit::ZERO, "write of {v} refused");
    }
    dut.wclk.falling().await;
    dut.winc.set(0);
}

/// Reads one word per `rclk` cycle and checks it against `values`, in order; each read
/// must find the FIFO non-empty (`rempty` low at its edge).
async fn read_words(dut: Dut, values: std::ops::Range<usize>) {
    for v in values {
        dut.rclk.falling().await;
        dut.rinc.set(1);
        dut.rclk.rising().await;
        assert_eq!(dut.rempty.get(), Bit::ZERO, "read of {v} on an empty FIFO");
        assert_eq!(
            dut.rdata.get(),
            Word::new_truncating(v as u128),
            "read #{v}"
        );
    }
    dut.rclk.falling().await;
    dut.rinc.set(0);
}

/// Directed: with the reader idle, the FIFO takes exactly `DEPTH` writes, then holds
/// `wfull`, and the read side, no longer empty, shows the oldest word.
#[simjig::test(timeout = "10us")]
async fn full_after_depth_writes(dut: Dut) {
    start_clocks(dut, directed_clocks());
    reset(dut).await;
    let mut accepted = 0;
    for i in 0..4 * DEPTH {
        dut.wclk.falling().await;
        dut.winc.set(1);
        dut.wdata.set(Word::new_truncating(i as u128));
        dut.wclk.rising().await;
        if !dut.wfull.get().to_bool() {
            accepted += 1;
        }
    }
    assert_eq!(accepted, DEPTH, "writes accepted before wfull");
    ReadOnly::new().await;
    assert_eq!(
        dut.wfull.get(),
        Bit::ONE,
        "wfull holds while nothing is read"
    );
    assert_eq!(dut.rempty.get(), Bit::ZERO, "the read side sees the writes");
    assert_eq!(
        dut.rdata.get(),
        Word::new(0),
        "the oldest word is shown first"
    );
}

/// Directed: each flag sees the other side's pointer three of its own clock's edges after
/// the pointer moved (two synchronizer flops, then the flag's flop). One write clears
/// `rempty` at the third `rclk` edge after it; with the FIFO full, one read clears `wfull`
/// at the third `wclk` edge after it.
#[simjig::test(timeout = "10us")]
async fn flags_see_the_other_side_three_edges_later(dut: Dut) {
    start_clocks(dut, directed_clocks());
    reset(dut).await;
    dut.wclk.falling().await;
    dut.winc.set(1);
    dut.wclk.rising().await; // the first write
    assert_eq!(edges_until_low(dut.rclk, dut.rempty).await, 3, "rempty");
    for _ in 1..DEPTH {
        dut.wclk.rising().await;
    }
    dut.wclk.falling().await;
    dut.winc.set(0);
    dut.wclk.rising().await;
    ReadOnly::new().await;
    assert_eq!(dut.wfull.get(), Bit::ONE, "full after DEPTH writes");
    dut.rclk.falling().await;
    dut.rinc.set(1);
    dut.rclk.rising().await; // the first read
    dut.rinc.set(0);
    assert_eq!(edges_until_low(dut.wclk, dut.wfull).await, 3, "wfull");
}

/// Counts the rising edges of `clk` until `flag` is low after one, from now.
async fn edges_until_low(clk: In<Bit>, flag: Out<Bit>) -> u32 {
    for edges in 1..=8 {
        clk.rising().await;
        ReadOnly::new().await;
        if !flag.get().to_bool() {
            return edges;
        }
    }
    panic!("`{}` still high eight edges later", flag.name());
}

/// The elaboration checks: power-of-2 depths from 2 up elaborate, other depths and a WIDTH
/// below 1 stop elaboration.
#[test]
fn parameters_are_checked_at_elaboration() {
    let src = &["rtl/async_fifo.sv"];
    let good: &[&[&str]] = &[&["DEPTH=2"], &["DEPTH=64", "WIDTH=1"]];
    let bad_depth: &[&[&str]] = &[&["DEPTH=1"], &["DEPTH=12"]];
    common::assert_param_checks(
        "async_fifo",
        src,
        good,
        bad_depth,
        "DEPTH must be a power of 2",
    );
    let bad_width: &[&[&str]] = &[&["WIDTH=0"]];
    common::assert_param_checks(
        "async_fifo",
        src,
        &[],
        bad_width,
        "WIDTH must be at least 1",
    );
}
