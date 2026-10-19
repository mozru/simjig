//! `fifo`: a valid/ready stream with random back-pressure. A driver, a sink and the test
//! body run as tasks; the sink checks order against the data sent.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::{TestCtx, clock, ns, read_only, spawn};
use simjig_bits::Bit;

use crate::fifo;

const BEATS: u32 = 40;

#[simjig::test(timeout = "10us")]
async fn every_beat_arrives_in_order_under_back_pressure(ctx: &TestCtx, dut: fifo::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.in_valid.set(0);
    dut.out_ready.set(0);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);

    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut gaps = ctx.rng("driver");
    let log = Rc::clone(&sent);
    // Inputs change on falling edges; a beat moves at the rising edge where
    // valid && ready, which ReadOnly before that edge's eval cannot see, so the driver
    // samples `in_ready` in the ReadOnly before the edge.
    let driver = spawn("driver", async move {
        let mut next = 0u32;
        while next < BEATS {
            let valid = gaps.below(4) != 0;
            dut.in_valid.set(valid);
            dut.in_data.set(u128::from(next));
            read_only().await;
            if valid && dut.in_ready.get() == Bit::ONE {
                log.borrow_mut().push(next);
                next += 1;
            }
            dut.clk.falling().await;
        }
        dut.in_valid.set(0);
    });
    let mut stalls = ctx.rng("sink");
    let sink = spawn("sink", async move {
        let mut got = Vec::new();
        let mut full_seen = false;
        while got.len() < BEATS as usize {
            let ready = stalls.below(3) == 0;
            dut.out_ready.set(ready);
            read_only().await;
            full_seen |= dut.in_ready.get() == Bit::ZERO;
            if ready && dut.out_valid.get() == Bit::ONE {
                got.push(u32::try_from(dut.out_data.get().to_u128()).unwrap());
            }
            dut.clk.falling().await;
        }
        (got, full_seen)
    });
    let (got, full_seen) = sink.await;
    driver.await;
    assert_eq!(got, *sent.borrow());
    assert_eq!(got, (0..BEATS).collect::<Vec<_>>());
    assert!(
        full_seen,
        "the sink's stalls should fill the 4-deep FIFO at least once"
    );
}
