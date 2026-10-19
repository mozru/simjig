//! The simjig flow of the benchmark's mid-size design (`bench/rtl/mid_pipe.sv`;
//! `bench/README.md`, "Designs"): a 64-bit FIFO of 256 entries feeding a 128-stage
//! pipeline whose output is the popped entry again, with a CRC alongside. The same three
//! variants and the same timing rule as `bench.rs`; `bench/cocotb/test_mid.py`,
//! `bench/marlin/tests/mid.rs` and `bench/cpp/mid.cpp` are the same testbench:
//! - `clock_only` (A): the clock alone;
//! - `monitor_only` (B): A plus a monitor reading seven signals per rising edge into a
//!   hash;
//! - `full` (C): a reset, then a producer (random pushes of random 64-bit words), a
//!   consumer (random pops) and a monitor with a scoreboard: it checks `count`, `full`
//!   and `empty` against a model of the FIFO, and `out_valid` and `out_data` against the
//!   entries it saw popped, each due `LATENCY` samples after its pop. The hash is of every
//!   `out_crc`, in order.
//!
//! Each variant also has a synchronous twin (`*_sync`, the flow `simjig-sync`): the same
//! testbench as a plain loop over `SyncClock::tick` (`bench.rs` explains the timing).
#![expect(
    clippy::disallowed_methods,
    reason = "a benchmark measures wall-clock time"
)]

mod common;

use std::collections::VecDeque;
use std::time::Instant;

use common::{CONSUMER_SEED, PHASE, PRODUCER_SEED, XorShift32, bit, cycles, report_flow};
use simjig::bits::Bit;
use simjig::{SyncClock, clock, ns, spawn};

simjig::include_dut!(bench_mid); // the sweep: bench_mid::{Ports, mid, mid_pub}

/// Entries in the FIFO (`mid_pipe`'s `DEPTH`).
const DEPTH: usize = 256;
/// Samples from an entry's pop to its `out_valid`: `mid_pipe`'s `STAGES` plus one (the
/// RTL's "Timing" comment).
const LATENCY: u64 = 128 + 1;

/// The DUT's name in the report: the sweep member's module path.
fn dut_name<D>() -> &'static str {
    let name = std::any::type_name::<D>();
    if name.contains("mid_pub") {
        "mid_pub"
    } else {
        "mid"
    }
}

/// The two halves of a 64-bit word, folded into one.
fn fold64(x: u128) -> u32 {
    (x as u32) ^ ((x >> 32) as u32)
}

/// (A) The clock alone.
async fn clock_only<D: bench_mid::Ports>(dut: D, flow: &str) {
    let n = cycles();
    let start = Instant::now();
    let clk = dut.clk();
    clock(clk, ns(10)).start();
    for _ in 0..n {
        clk.rising().await;
    }
    report_flow(flow, dut_name::<D>(), "clock_only", n, start, r#""hash":0"#);
}

/// (B) A monitor reading seven signals per rising edge.
async fn monitor_only<D: bench_mid::Ports>(dut: D, flow: &str) {
    let n = cycles();
    let start = Instant::now();
    let clk = dut.clk();
    clock(clk, ns(10)).start();
    let (push, pop, full, empty) = (dut.push(), dut.pop(), dut.full(), dut.empty());
    let (count, valid, data) = (dut.count(), dut.out_valid(), dut.out_data());
    let monitor = spawn("monitor", async move {
        let mut hash = 0u32;
        for _ in 0..n {
            clk.rising().await;
            let s = bit(push.get())
                + 2 * bit(pop.get())
                + 4 * bit(full.get())
                + 8 * bit(empty.get())
                + 16 * count.get().to_u128() as u32
                + 16384 * bit(valid.get());
            hash = hash.wrapping_mul(31).wrapping_add(s);
            hash = hash
                .wrapping_mul(31)
                .wrapping_add(fold64(data.get().to_u128()));
        }
        hash
    });
    let hash = monitor.await;
    report_flow(
        flow,
        dut_name::<D>(),
        "monitor_only",
        n,
        start,
        &format!(r#""hash":{hash}"#),
    );
}

/// The full testbench's scoreboard and what it counted.
#[derive(Default)]
struct Scoreboard {
    entries: VecDeque<u128>,
    /// Entries popped and inside the pipeline: (the sample they are due at, the entry).
    pipe: VecDeque<(u64, u128)>,
    pushes: u64,
    pops: u64,
    /// A hash of every `out_crc`, in order.
    hash: u32,
}

/// What the monitor reads at every rising edge (the pre-edge values); it reads
/// `out_data` and `out_crc` only when an entry is due, as the other flows do.
struct Sample {
    push: bool,
    pop: bool,
    wr_data: u128,
    count: usize,
    full: bool,
    empty: bool,
    out_valid: bool,
}

impl Scoreboard {
    /// The monitor at sample `i` (just before rising edge `i`): checks the pre-edge
    /// outputs (`out` reads `out_data` and `out_crc`), then applies the edge's push and
    /// pop.
    fn sample(&mut self, i: u64, s: Sample, out: impl FnOnce() -> (u128, u32)) {
        let len = self.entries.len();
        assert_eq!(s.count, len, "count");
        assert_eq!(s.full, len == DEPTH, "full");
        assert_eq!(s.empty, len == 0, "empty");
        let due = self.pipe.front().is_some_and(|&(at, _)| at == i);
        assert_eq!(s.out_valid, due, "out_valid");
        if due {
            let (_, want) = self.pipe.pop_front().expect("due");
            let (data, crc) = out();
            assert_eq!(data, want, "out_data");
            self.hash = self.hash.wrapping_mul(31).wrapping_add(crc);
        }
        let do_pop = s.pop && len > 0;
        let do_push = s.push && (len < DEPTH || do_pop);
        if do_pop {
            let e = self.entries.pop_front().expect("len > 0");
            self.pipe.push_back((i + LATENCY, e));
            self.pops += 1;
        }
        if do_push {
            self.entries.push_back(s.wr_data);
            self.pushes += 1;
        }
    }
}

/// (C) Reset, producer, consumer, and a monitor with a scoreboard.
async fn full<D: bench_mid::Ports>(dut: D, flow: &str) {
    let n = cycles();
    let start = Instant::now();
    let clk = dut.clk();
    let (rst, push, wr, pop) = (dut.rst(), dut.push(), dut.wr_data(), dut.pop());
    let (full, empty, count) = (dut.full(), dut.empty(), dut.count());
    let (valid, data, crc) = (dut.out_valid(), dut.out_data(), dut.out_crc());
    clock(clk, ns(10)).start();
    // Reset: two edges with every input idle, released at a falling edge.
    rst.set(1);
    push.set(0);
    pop.set(0);
    clk.rising().await;
    clk.rising().await;
    clk.falling().await;
    rst.set(0);

    let producer = spawn("producer", async move {
        let (mut rng, mut rate) = (XorShift32(PRODUCER_SEED), 0);
        for i in 0..n {
            if i % PHASE == 0 {
                rate = rng.rate();
            }
            clk.falling().await;
            push.set(rng.next() % 100 < rate);
            wr.set(u128::from(rng.next64()));
        }
    });
    let consumer = spawn("consumer", async move {
        let (mut rng, mut rate) = (XorShift32(CONSUMER_SEED), 0);
        for i in 0..n {
            if i % PHASE == 0 {
                rate = rng.rate();
            }
            clk.falling().await;
            pop.set(rng.next() % 100 < rate);
        }
    });
    let monitor = spawn("monitor", async move {
        let mut sb = Scoreboard {
            entries: VecDeque::with_capacity(DEPTH),
            ..Scoreboard::default()
        };
        for i in 0..n {
            clk.rising().await;
            sb.sample(
                i,
                Sample {
                    push: push.get() == Bit::ONE,
                    pop: pop.get() == Bit::ONE,
                    wr_data: wr.get().to_u128(),
                    count: count.get().to_u128() as usize,
                    full: full.get() == Bit::ONE,
                    empty: empty.get() == Bit::ONE,
                    out_valid: valid.get() == Bit::ONE,
                },
                || (data.get().to_u128(), crc.get().to_u128() as u32),
            );
        }
        sb
    });
    producer.await;
    consumer.await;
    let sb = monitor.await;
    let result = format!(
        r#""pushes":{},"pops":{},"hash":{}"#,
        sb.pushes, sb.pops, sb.hash
    );
    report_flow(flow, dut_name::<D>(), "full", n, start, &result);
}

#[simjig::test(timeout = "none", duts = [bench_mid::mid::Dut, bench_mid::mid_pub::Dut])]
async fn a_clock_only<D: bench_mid::Ports>(dut: D) {
    clock_only(dut, "simjig").await;
}

#[simjig::test(timeout = "none", duts = [bench_mid::mid::Dut, bench_mid::mid_pub::Dut])]
async fn b_monitor_only<D: bench_mid::Ports>(dut: D) {
    monitor_only(dut, "simjig").await;
}

#[simjig::test(timeout = "none", duts = [bench_mid::mid::Dut, bench_mid::mid_pub::Dut])]
async fn c_full<D: bench_mid::Ports>(dut: D) {
    full(dut, "simjig").await;
}

// The same three with eval elision opted in (`elide_evals`): a separately labelled flow,
// `simjig-elide`, to measure what the default (every eval runs) costs. Safe for this
// design: it writes none of its inputs and reads no `$time`, prints nothing and calls no
// DPI-C (`ModelOptions::with_elide_evals`).

#[simjig::test(timeout = "none", elide_evals, duts = [bench_mid::mid::Dut])]
async fn a_clock_only_elide<D: bench_mid::Ports>(dut: D) {
    clock_only(dut, "simjig-elide").await;
}

#[simjig::test(timeout = "none", elide_evals, duts = [bench_mid::mid::Dut])]
async fn b_monitor_only_elide<D: bench_mid::Ports>(dut: D) {
    monitor_only(dut, "simjig-elide").await;
}

#[simjig::test(timeout = "none", elide_evals, duts = [bench_mid::mid::Dut])]
async fn c_full_elide<D: bench_mid::Ports>(dut: D) {
    full(dut, "simjig-elide").await;
}

// The synchronous flow (`#[simjig::test(sync)]`, `SyncClock`), as in `bench.rs`: one
// `tick()` per cycle, inputs set right after the falling edge, outputs read just before the
// rising one. Every output of `mid_pipe` is registered (its FIFO's `count`, `full` and
// `empty` too: `sync_fifo.sv`), so each read sees what the async monitor sees and the
// results must be equal.

/// (A, sync) The clock alone.
fn clock_only_sync<D: bench_mid::Ports>(dut: &D) {
    let n = cycles();
    let start = Instant::now();
    let mut cyc = SyncClock::new(dut.clk(), ns(10));
    for _ in 0..n {
        cyc.tick();
    }
    report_flow(
        "simjig-sync",
        dut_name::<D>(),
        "clock_only",
        n,
        start,
        r#""hash":0"#,
    );
}

/// (B, sync) Seven signals read before every rising edge.
fn monitor_only_sync<D: bench_mid::Ports>(dut: &D) {
    let n = cycles();
    let start = Instant::now();
    let mut cyc = SyncClock::new(dut.clk(), ns(10));
    let (push, pop, full, empty) = (
        cyc.bind(dut.push()),
        cyc.bind(dut.pop()),
        cyc.bind(dut.full()),
        cyc.bind(dut.empty()),
    );
    let (count, valid, data) = (
        cyc.bind(dut.count()),
        cyc.bind(dut.out_valid()),
        cyc.bind(dut.out_data()),
    );
    let mut hash = 0u32;
    for _ in 0..n {
        let s = bit(push.get())
            + 2 * bit(pop.get())
            + 4 * bit(full.get())
            + 8 * bit(empty.get())
            + 16 * count.get().to_u128() as u32
            + 16384 * bit(valid.get());
        hash = hash.wrapping_mul(31).wrapping_add(s);
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(fold64(data.get().to_u128()));
        cyc.tick();
    }
    let result = format!(r#""hash":{hash}"#);
    report_flow(
        "simjig-sync",
        dut_name::<D>(),
        "monitor_only",
        n,
        start,
        &result,
    );
}

/// (C, sync) Reset, then each cycle: the monitor before the rising edge, tick, the
/// drivers after the falling edge.
fn full_sync<D: bench_mid::Ports>(dut: &D) {
    let n = cycles();
    let start = Instant::now();
    let mut cyc = SyncClock::new(dut.clk(), ns(10));
    let (rst, push, wr, pop) = (
        cyc.bind(dut.rst()),
        cyc.bind(dut.push()),
        cyc.bind(dut.wr_data()),
        cyc.bind(dut.pop()),
    );
    let (full, empty, count) = (
        cyc.bind(dut.full()),
        cyc.bind(dut.empty()),
        cyc.bind(dut.count()),
    );
    let (valid, data, crc) = (
        cyc.bind(dut.out_valid()),
        cyc.bind(dut.out_data()),
        cyc.bind(dut.out_crc()),
    );
    // Reset: two rising edges with every input idle, released after a falling edge.
    rst.set(1);
    push.set(0);
    pop.set(0);
    cyc.tick();
    cyc.tick();
    rst.set(0);
    let (mut prng, mut prate) = (XorShift32(PRODUCER_SEED), 0);
    let (mut crng, mut crate_) = (XorShift32(CONSUMER_SEED), 0);
    let mut sb = Scoreboard {
        entries: VecDeque::with_capacity(DEPTH),
        ..Scoreboard::default()
    };
    for i in 0..n {
        sb.sample(
            i,
            Sample {
                push: push.get() == Bit::ONE,
                pop: pop.get() == Bit::ONE,
                wr_data: wr.get().to_u128(),
                count: count.get().to_u128() as usize,
                full: full.get() == Bit::ONE,
                empty: empty.get() == Bit::ONE,
                out_valid: valid.get() == Bit::ONE,
            },
            || (data.get().to_u128(), crc.get().to_u128() as u32),
        );
        cyc.tick();
        if i % PHASE == 0 {
            prate = prng.rate();
            crate_ = crng.rate();
        }
        push.set(prng.next() % 100 < prate);
        wr.set(u128::from(prng.next64()));
        pop.set(crng.next() % 100 < crate_);
    }
    let result = format!(
        r#""pushes":{},"pops":{},"hash":{}"#,
        sb.pushes, sb.pops, sb.hash
    );
    report_flow("simjig-sync", dut_name::<D>(), "full", n, start, &result);
}

#[simjig::test(sync, timeout = "none", duts = [bench_mid::mid::Dut])]
fn a_clock_only_sync<D: bench_mid::Ports>(dut: D) {
    clock_only_sync(&dut);
}

#[simjig::test(sync, timeout = "none", duts = [bench_mid::mid::Dut])]
fn b_monitor_only_sync<D: bench_mid::Ports>(dut: D) {
    monitor_only_sync(&dut);
}

#[simjig::test(sync, timeout = "none", duts = [bench_mid::mid::Dut])]
fn c_full_sync<D: bench_mid::Ports>(dut: D) {
    full_sync(&dut);
}
