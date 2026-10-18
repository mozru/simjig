//! Testbench for `rtl/async_fifo.sv`, built with `WIDTH = 8`, `DEPTH = 8`.
//!
//! Two kernel clocks with unrelated periods drive the two domains; each domain has its own
//! reset, released on its own clock. The testbench is split by role, one task each:
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
//! data are checked per read, and the test ends only when every written item was read.
//!
//! Monitors sample at `rising()`, which for a testbench clock shows the pre-edge values:
//! the `winc`, `wfull` and `rdata` the edge acts on.
#![forbid(unsafe_code)]

use std::cell::Cell;
use std::rc::Rc;

use simjig::bits::{Bit, Bits};
use simjig::{
    Event, In, Out, Queue, ReadOnly, Rng, SimDuration, TestCtx, clock, cycles, join, ns, ps, spawn,
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

/// Holds one domain's asynchronous reset for three of its clock's cycles and releases it
/// just after a falling edge of that clock, away from the rising edge its flops use.
async fn reset_domain(clk: In<Bit>, rst_n: In<Bit>) {
    rst_n.set(0);
    cycles(clk, 3).await;
    clk.falling().await;
    rst_n.set(1);
}

/// Idles both sides and resets both domains, each on its own clock, concurrently.
async fn reset(dut: Dut) {
    dut.winc.set(0);
    dut.wdata.set(0);
    dut.rinc.set(0);
    join!(
        reset_domain(dut.wclk, dut.wrst_n),
        reset_domain(dut.rclk, dut.rrst_n)
    );
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
            dut.winc.set(0);
            dut.wclk.rising().await;
        }
        dut.winc.set(1);
        dut.wdata.set(Word::new_truncating(data.next_u64().into()));
        dut.wclk.rising().await;
        // Pre-edge `wfull`: whether this edge took the word.
        while dut.wfull.get().to_bool() {
            dut.wclk.rising().await;
        }
    }
    dut.winc.set(0);
}

/// The reader: drives `rinc` with random stalls, forever (it is detached).
async fn reader(dut: Dut, mut stalls: Stalls) {
    loop {
        dut.rinc.set(stalls.active());
        dut.rclk.rising().await;
    }
}

/// What the monitors counted, to show the test reached both boundaries.
#[derive(Debug, Default)]
struct Coverage {
    /// `wclk` edges with `winc` high that `wfull` refused.
    writes_refused: Cell<u32>,
    /// `rclk` edges with `rinc` high that `rempty` refused.
    reads_refused: Cell<u32>,
    reads: Cell<u32>,
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
    }
}

/// Checks every read the FIFO performs against the scoreboard, in order, and sets
/// `drained` after the `ITEMS`-th.
async fn read_monitor(dut: Dut, scoreboard: Queue<Word>, coverage: Rc<Coverage>, drained: Event) {
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
        if coverage.reads.get() == ITEMS {
            drained.set();
        }
    }
}

/// Runs `ITEMS` words through the FIFO under random stalls on both sides and checks
/// order, loss and both flags; returns what the monitors counted.
async fn traffic(ctx: &TestCtx, dut: Dut, clocks: Clocks) -> Rc<Coverage> {
    start_clocks(dut, clocks);
    reset(dut).await;
    let scoreboard = Queue::new("scoreboard", DEPTH);
    let coverage = Rc::new(Coverage::default());
    let drained = Event::new("drained");
    let monitor = write_monitor(dut, scoreboard.clone(), Rc::clone(&coverage));
    spawn("write monitor", monitor).detach();
    let monitor = read_monitor(
        dut,
        scoreboard.clone(),
        Rc::clone(&coverage),
        drained.clone(),
    );
    spawn("read monitor", monitor).detach();
    spawn("reader", reader(dut, Stalls::new(ctx.rng("read stalls")))).detach();
    let writer = spawn(
        "writer",
        writer(dut, ctx.rng("data"), Stalls::new(ctx.rng("write stalls"))),
    );
    let all_read = async {
        writer.await;
        drained.wait().await;
    };
    // Far more than the slowest ratio needs: a lost item fails here, not at the test's limit.
    if let Err(elapsed) = with_timeout(us(400), all_read).await {
        let read = coverage.reads.get();
        panic!("{read} of {ITEMS} items read back: {elapsed}");
    }
    assert!(scoreboard.is_empty(), "items read beyond those written");
    coverage
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
    let coverage = traffic(ctx, dut, clocks).await;
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
    let coverage = traffic(ctx, dut, clocks).await;
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
    let coverage = traffic(ctx, dut, clocks).await;
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
    let coverage = traffic(ctx, dut, clocks).await;
    assert_ran_dry(&coverage);
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

/// Directed: with the reader idle, the FIFO takes exactly `DEPTH` writes, then holds
/// `wfull` and shows the oldest word.
#[simjig::test(timeout = "10us")]
async fn full_after_depth_writes(dut: Dut) {
    start_clocks(dut, directed_clocks());
    reset(dut).await;
    let mut accepted = 0;
    dut.winc.set(1);
    for i in 0..4 * DEPTH {
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
    dut.winc.set(1);
    dut.wclk.rising().await; // the first write
    assert_eq!(edges_until_low(dut.rclk, dut.rempty).await, 3, "rempty");
    for _ in 1..DEPTH {
        dut.wclk.rising().await;
    }
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
