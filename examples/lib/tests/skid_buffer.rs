//! `skid_buffer` (`rtl/skid_buffer.sv`): a valid/ready pipeline register whose ports are
//! `stream_if` modports. `build.rs` binds them (`bind_iface_with`), so `dut.up` and
//! `dut.down` are bundles typed from the modports: the testbench writes `up.valid` and
//! `up.data` and reads `up.ready`; on `down` it is the reverse.
//!
//! As in `tests/sync_fifo.rs`, tasks drive on the falling edge and sample on the rising
//! edge, where signals still hold the values the flops capture. The [`Board`] scoreboard
//! keeps the beats in flight: each beat taken on `up` must leave on `down` exactly once, in
//! order, and never more than two at a time (the buffer's two registers).
#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions: a panic is the test failure"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use simjig::bits::{Bit, Bits};
use simjig::{ReadOnly, Rng, TestCtx, clock, cycles, ns, spawn, us, with_timeout};

mod common;

simjig::include_dut!(skid_buffer);

use skid_buffer::Dut;

/// One beat of the 16-bit stream.
type Beat = Bits<16>;

/// Beats per random test.
const BEATS: usize = 1000;
/// The sink redraws its `ready` rate every `PHASE` cycles, so stalls come in bursts.
const PHASE: u32 = 16;

/// The scoreboard: what the monitor saw on both sides.
#[derive(Debug, Default)]
struct Board {
    /// Beats taken on `up` that have not left on `down`, oldest first.
    in_flight: VecDeque<Beat>,
    /// Beats that left on `down`.
    received: Vec<Beat>,
    /// `down` as sampled at the previous edge, while it was stalled (`valid && !ready`).
    stalled: Option<Beat>,
    /// Edges where `down` stalled, and where `up` offered a beat but was not ready.
    down_stalls: u32,
    up_blocked: u32,
    /// The edges (counted from reset) of the first and last beat out.
    first_out: Option<u32>,
    last_out: u32,
    edges: u32,
}

impl Board {
    /// Checks one edge's handshakes and applies them.
    fn step(&mut self, dut: Dut) {
        let up = (dut.up.valid.get().to_bool(), dut.up.ready.get().to_bool());
        let down = (
            dut.down.valid.get().to_bool(),
            dut.down.ready.get().to_bool(),
        );
        let down_data = dut.down.data.get();
        if let Some(held) = self.stalled.take() {
            assert!(down.0, "down.valid dropped while stalled");
            assert_eq!(down_data, held, "down.data changed while stalled");
        }
        // A beat leaving now was taken at an earlier edge: there is no zero-cycle path.
        if down.0 && down.1 {
            let want = self.in_flight.pop_front();
            assert_eq!(Some(down_data), want, "beat {} out", self.received.len());
            self.received.push(down_data);
            self.first_out.get_or_insert(self.edges);
            self.last_out = self.edges;
        }
        if down.0 && !down.1 {
            self.stalled = Some(down_data);
            self.down_stalls += 1;
        }
        if up.0 && up.1 {
            self.in_flight.push_back(dut.up.data.get());
        }
        self.up_blocked += u32::from(up.0 && !up.1);
        assert!(
            self.in_flight.len() <= 2,
            "{} beats in flight",
            self.in_flight.len()
        );
        self.edges += 1;
    }
}

/// Starts a monitor that steps the board on every rising edge until the test ends.
fn monitor(dut: Dut) -> Rc<RefCell<Board>> {
    let board = Rc::new(RefCell::new(Board::default()));
    let shared = Rc::clone(&board);
    spawn("monitor", async move {
        loop {
            dut.clk.rising().await;
            shared.borrow_mut().step(dut);
        }
    })
    .detach();
    board
}

/// Holds reset for two cycles with `up` idle and `down` not ready; returns just after the
/// last reset edge.
async fn reset(dut: Dut) {
    dut.rst.set(1);
    dut.up.valid.set(0);
    dut.down.ready.set(0);
    cycles(dut.clk, 2).await;
    dut.rst.set(0);
}

/// Starts the clock, resets, and starts the monitor.
async fn start(dut: Dut) -> Rc<RefCell<Board>> {
    clock(dut.clk, ns(10)).start();
    reset(dut).await;
    monitor(dut)
}

/// Sends `beats` in order on `up`. A beat not yet offered is offered with probability
/// `rate`%; once offered, `valid` and `data` hold until the beat moves (the `stream_if`
/// rule), whatever `ready` does.
async fn source(dut: Dut, beats: Vec<Beat>, mut rng: Rng, rate: u64) {
    let mut offered = false;
    for beat in beats {
        loop {
            dut.clk.falling().await;
            offered = offered || rng.below(100) < rate;
            dut.up.valid.set(offered);
            dut.up.data.set(beat);
            dut.clk.rising().await;
            if offered && dut.up.ready.get() == Bit::ONE {
                offered = false;
                break;
            }
        }
    }
    dut.clk.falling().await;
    dut.up.valid.set(0);
}

/// Drives `down.ready` at a random rate, redrawn every [`PHASE`] cycles, until the test
/// ends.
async fn sink(dut: Dut, mut rng: Rng) {
    let mut rate = 0;
    for n in (0..PHASE).cycle() {
        if n == 0 {
            rate = [5, 50, 95][usize::try_from(rng.below(3)).unwrap()];
        }
        dut.clk.falling().await;
        dut.down.ready.set(rng.below(100) < rate);
    }
}

/// Waits until `count` beats have left on `down`.
async fn received(dut: Dut, board: &RefCell<Board>, count: usize) {
    while board.borrow().received.len() < count {
        dut.clk.rising().await;
        ReadOnly::new().await;
    }
}

fn random_beats(rng: &mut Rng, count: usize) -> Vec<Beat> {
    (0..count)
        .map(|_| Bits::new(u128::from(rng.below(1 << 16))))
        .collect()
}

/// Random `valid` and random `ready`: every beat arrives once, in order (the board checks
/// each), and the backpressure reached both registers.
#[simjig::test(timeout = "200us")]
async fn random_valid_and_ready_lose_nothing(ctx: &TestCtx, dut: Dut) {
    let board = start(dut).await;
    let beats = random_beats(&mut ctx.rng("beats"), BEATS);
    spawn("sink", sink(dut, ctx.rng("sink"))).detach();
    spawn("source", source(dut, beats.clone(), ctx.rng("source"), 70)).await;
    with_timeout(us(5), received(dut, &board, BEATS))
        .await
        .expect("every beat leaves within 5 us of the last one sent");
    let board = board.borrow();
    assert_eq!(board.received, beats);
    assert!(board.in_flight.is_empty());
    assert!(board.down_stalls > 0 && board.up_blocked > 0, "{board:?}");
}

/// With `down.ready` held high and a beat offered every cycle, one beat moves per cycle:
/// `up.ready` never drops, and the last beat leaves `BEATS - 1` edges after the first.
#[simjig::test(timeout = "20us")]
async fn full_throughput_while_ready_is_high(ctx: &TestCtx, dut: Dut) {
    let board = start(dut).await;
    let beats = random_beats(&mut ctx.rng("beats"), BEATS);
    dut.down.ready.set(1);
    spawn("source", source(dut, beats.clone(), ctx.rng("source"), 100)).await;
    with_timeout(us(1), received(dut, &board, BEATS))
        .await
        .expect("the last beat leaves within 1 us");
    let board = board.borrow();
    assert_eq!(board.received, beats);
    assert_eq!(board.up_blocked, 0, "up.ready dropped");
    let first = board.first_out.unwrap();
    assert_eq!(board.last_out - first, u32::try_from(BEATS - 1).unwrap());
}

/// A stalled `down`: the output register takes the first beat and the skid the second;
/// only then does `up.ready` drop, holding off the third. Released, all three leave in
/// order, one per cycle, and `up.ready` returns once the skid has emptied.
#[simjig::test(timeout = "2us")]
async fn a_stall_fills_the_skid_then_drains_in_order(dut: Dut) {
    let board = start(dut).await;
    let beats: Vec<Beat> = [0xa1, 0xb2, 0xc3].map(Bits::new).to_vec();
    let mut ready = Vec::new();
    for &beat in &beats {
        dut.clk.falling().await;
        dut.up.valid.set(1);
        dut.up.data.set(beat);
        dut.clk.rising().await;
        ready.push(dut.up.ready.get().to_bool()); // as the edge sampled it
    }
    assert_eq!(
        ready,
        [true, true, false],
        "up.ready drops only when the skid is full"
    );
    assert_eq!(board.borrow().in_flight, &beats[..2]);
    dut.clk.falling().await;
    dut.down.ready.set(1);
    // `up` still offers the third beat. The first released edge moves the skid into the
    // output register, and `up.ready` returns at the next one, which takes the beat.
    let mut waited = 0;
    loop {
        dut.clk.rising().await;
        waited += 1;
        if dut.up.ready.get() == Bit::ONE {
            break;
        }
    }
    assert_eq!(waited, 2, "edges until the third beat moves");
    dut.clk.falling().await;
    dut.up.valid.set(0);
    with_timeout(us(1), received(dut, &board, 3))
        .await
        .expect("the three beats leave");
    assert_eq!(board.borrow().received, beats);
}

/// The elaboration checks: the module lints cleanly over default-width interfaces, and
/// stops on a WIDTH below 1 or one that differs from the interfaces' own `WIDTH`.
#[test]
fn parameters_are_checked_at_elaboration() {
    let src = &["rtl/stream_if.sv", "rtl/skid_buffer.sv"];
    // As a top, the module's interface ports get `stream_if` instances with WIDTH = 8.
    let good: &[&[&str]] = &[&[], &["WIDTH=8"]];
    common::assert_param_checks(
        "skid_buffer",
        src,
        good,
        &[&["WIDTH=0"]],
        "WIDTH must be at least 1",
    );
    let bad: &[&[&str]] = &[&["WIDTH=16"]];
    common::assert_param_checks("skid_buffer", src, &[], bad, "but up.data has 8 bits");
}
