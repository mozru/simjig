//! (a) `async_fifo` end to end: a writer on `wclk` and a reader on an unrelated `rclk`
//! (kernel clocks of 10 ns and 13 ns) move a random stream through the Gray-coded pointer
//! synchronizers; every word arrives once, in order, and the probed pointers never claim
//! more than `DEPTH` words in flight.

use simjig::{TestCtx, clock, ns, spawn};
use simjig_bits::Bit;

use crate::async_fifo::Dut;

const WORDS: usize = 300;
/// `DEPTH` in build.rs.
const DEPTH: u128 = 8;

/// Both resets for three cycles of the slower clock (the design asks for two), released
/// between edges.
async fn reset(dut: Dut) {
    dut.wrst.set(1);
    dut.rrst.set(1);
    dut.winc.set(0);
    dut.rinc.set(0);
    for _ in 0..3 {
        dut.rclk.rising().await;
    }
    dut.rclk.falling().await;
    dut.wrst.set(0);
    dut.rrst.set(0);
}

/// Offers `words` on the write side: at each falling `wclk`, the next word if `wfull` is
/// low (it is registered on the rising edge, so it is stable here).
async fn write(dut: Dut, words: Vec<u8>) {
    let mut next = words.into_iter().peekable();
    while let Some(&word) = next.peek() {
        dut.wclk.falling().await;
        if dut.wfull.get() == Bit::ZERO {
            dut.winc.set(1);
            dut.wdata.set(u128::from(word));
            next.next();
        } else {
            dut.winc.set(0);
        }
    }
    dut.wclk.falling().await;
    dut.winc.set(0);
}

#[simjig::test(timeout = "200us")]
async fn a_random_stream_crosses_two_unrelated_clocks_intact(ctx: &TestCtx, dut: Dut) {
    let mut rng = ctx.rng("data");
    let words: Vec<u8> = (0..WORDS)
        .map(|_| u8::try_from(rng.below(256)).unwrap())
        .collect();
    clock(dut.wclk, ns(10)).start();
    clock(dut.rclk, ns(13)).start();
    reset(dut).await;
    let writer = spawn("writer", write(dut, words.clone()));

    // The read side is first-word fall-through: at each falling `rclk`, a word is there
    // whenever `rempty` is low, and `rinc` takes it at the next rising edge.
    let mut received = Vec::with_capacity(WORDS);
    let mut full_seen = false;
    while received.len() < WORDS {
        dut.rclk.falling().await;
        let in_flight = (dut.wbin.get().to_u128() + 16 - dut.rbin.get().to_u128()) % 16;
        assert!(in_flight <= DEPTH, "{in_flight} words in flight");
        full_seen |= dut.wfull.get() == Bit::ONE;
        if dut.rempty.get() == Bit::ZERO {
            received.push(u8::try_from(dut.rdata.get().to_u128()).unwrap());
            dut.rinc.set(1);
        } else {
            dut.rinc.set(0);
        }
    }
    dut.rclk.falling().await;
    dut.rinc.set(0);
    writer.await;
    assert_eq!(received, words);
    // The 10 ns writer outruns the 13 ns reader, so the full flag throttled it.
    assert!(full_seen, "the FIFO never filled");
    dut.rclk.rising().await;
    dut.rclk.falling().await;
    assert_eq!(dut.rempty.get(), Bit::ONE, "nothing more arrives");
}
