//! The simjig half of the simjig-vs-cocotb benchmark (`bench/README.md`). Each test is one
//! variant; `bench/cocotb/test_bench.py` is the same testbench in cocotb, line for line:
//! - `clock_only` (A): the kernel clock runs and the test awaits `BENCH_CYCLES` rising
//!   edges. The cost of the scheduler, the clock and the model's eval.
//! - `monitor_only` (B): A plus a monitor task that reads six signals (`push`, `pop`,
//!   `full`, `empty`, `count`, `rd_data`) at every rising edge and folds them into a hash.
//! - `full` (C): a reset, then a producer (random pushes and bytes), a consumer (random
//!   pops) and a monitor checking every edge against a reference model of the FIFO.
//!
//! Timing rule (as in `examples/lib`): drivers write at the falling edge, the monitor
//! samples at the rising edge, where every signal still holds its pre-edge value (simjig
//! and cocotb agree: `tests/conformance/README.md`, "Edges on testbench-driven clocks").
//!
//! Stimulus comes from [`XorShift32`], not `ctx.rng`, so that both testbenches draw the
//! very same numbers: the final counts and the hash of popped bytes must then be equal in
//! the two flows, which shows they simulated the same thing.
//!
//! Each test measures its own body (model creation and the harness's report excluded) and
//! appends one JSON line to the file `BENCH_OUT` (stderr when unset).
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::io::Write as _;
use std::time::Instant;

use simjig::bits::Bit;
use simjig::{clock, ns, spawn};

simjig::include_dut!(bench_fifo); // the sweep: bench_fifo::{Ports, fifo, fifo_pub}

/// Entries in the FIFO (the `DEPTH` that `build.rs` sets).
const DEPTH: usize = 16;
/// A driver redraws its rate every `PHASE` cycles, from [`RATES`] (percent of cycles), so
/// the FIFO fills and drains in bursts instead of hovering at half full.
const PHASE: u64 = 24;
const RATES: [u32; 3] = [10, 50, 90];
/// Seeds of the producer's and the consumer's streams (any nonzero value).
const PRODUCER_SEED: u32 = 0x1234_5678;
const CONSUMER_SEED: u32 = 0x9E37_79B9;

/// Marsaglia's xorshift32 (13, 17, 5): three shifts, so the Python copy is cheap and
/// draws the same sequence.
struct XorShift32(u32);

impl XorShift32 {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// The rate for the next phase.
    fn rate(&mut self) -> u32 {
        RATES[(self.next() % 3) as usize]
    }
}

/// Cycles to run: `BENCH_CYCLES`, else 10 000.
fn cycles() -> u64 {
    std::env::var("BENCH_CYCLES").map_or(10_000, |v| v.parse().expect("BENCH_CYCLES"))
}

/// Appends this run's result to `BENCH_OUT` (or prints it).
fn report(dut: &str, variant: &str, n: u64, start: Instant, result: &str) {
    let secs = start.elapsed().as_secs_f64();
    let line = format!(
        r#"{{"flow":"simjig","dut":"{dut}","variant":"{variant}","cycles":{n},"seconds":{secs:.6},{result}}}"#
    );
    match std::env::var_os("BENCH_OUT") {
        Some(path) => {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("BENCH_OUT opens");
            writeln!(f, "{line}").expect("BENCH_OUT written");
        }
        None => eprintln!("{line}"),
    }
}

/// The DUT's name in the report: the sweep member's module path.
fn dut_name<D>() -> &'static str {
    let name = std::any::type_name::<D>();
    if name.contains("fifo_pub") { "fifo_pub" } else { "fifo" }
}

fn bit(b: Bit) -> u32 {
    u32::from(b == Bit::ONE)
}

/// (A) The clock alone.
async fn clock_only<D: bench_fifo::Ports>(dut: D) {
    let n = cycles();
    let start = Instant::now();
    let clk = dut.clk();
    clock(clk, ns(10)).start();
    for _ in 0..n {
        clk.rising().await;
    }
    report(dut_name::<D>(), "clock_only", n, start, r#""hash":0"#);
}

/// (B) A monitor reading six signals per rising edge.
async fn monitor_only<D: bench_fifo::Ports>(dut: D) {
    let n = cycles();
    let start = Instant::now();
    let clk = dut.clk();
    clock(clk, ns(10)).start();
    let (push, pop, full, empty, count, rd) = (
        dut.push(),
        dut.pop(),
        dut.full(),
        dut.empty(),
        dut.count(),
        dut.rd_data(),
    );
    let monitor = spawn("monitor", async move {
        let mut hash = 0u32;
        for _ in 0..n {
            clk.rising().await;
            let s = bit(push.get())
                + 2 * bit(pop.get())
                + 4 * bit(full.get())
                + 8 * bit(empty.get())
                + 16 * count.get().to_u128() as u32
                + 1024 * rd.get().to_u128() as u32;
            hash = hash.wrapping_mul(31).wrapping_add(s);
        }
        hash
    });
    let hash = monitor.await;
    report(dut_name::<D>(), "monitor_only", n, start, &format!(r#""hash":{hash}"#));
}

/// What the full testbench's monitor counted.
struct Stats {
    pushes: u64,
    pops: u64,
    /// A hash of every popped byte, in order.
    hash: u32,
}

/// (C) Reset, producer, consumer, and a monitor with a reference model.
async fn full<D: bench_fifo::Ports>(dut: D) {
    let n = cycles();
    let start = Instant::now();
    let clk = dut.clk();
    let (rst, push, wr, pop) = (dut.rst(), dut.push(), dut.wr_data(), dut.pop());
    let (full, empty, count, rd) = (dut.full(), dut.empty(), dut.count(), dut.rd_data());
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
            wr.set(u128::from(rng.next() & 0xFF));
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
        // The reference model: the entries, and what the registered `rd_data` shows (the
        // entry the last pop removed; zero after reset).
        let mut entries = VecDeque::with_capacity(DEPTH);
        let mut shown = 0u32;
        let mut st = Stats { pushes: 0, pops: 0, hash: 0 };
        for _ in 0..n {
            clk.rising().await;
            let (p, q, w) = (push.get() == Bit::ONE, pop.get() == Bit::ONE, wr.get());
            let len = entries.len();
            assert_eq!(count.get().to_u128() as usize, len, "count");
            assert_eq!(full.get() == Bit::ONE, len == DEPTH, "full");
            assert_eq!(empty.get() == Bit::ONE, len == 0, "empty");
            assert_eq!(rd.get().to_u128() as u32, shown, "rd_data");
            let do_pop = q && len > 0;
            let do_push = p && (len < DEPTH || do_pop);
            if do_pop {
                shown = entries.pop_front().expect("len > 0");
                st.pops += 1;
                st.hash = st.hash.wrapping_mul(31).wrapping_add(shown);
            }
            if do_push {
                entries.push_back(w.to_u128() as u32);
                st.pushes += 1;
            }
        }
        st
    });
    producer.await;
    consumer.await;
    let st = monitor.await;
    let result = format!(r#""pushes":{},"pops":{},"hash":{}"#, st.pushes, st.pops, st.hash);
    report(dut_name::<D>(), "full", n, start, &result);
}

#[simjig::test(timeout = "none", duts = [bench_fifo::fifo::Dut, bench_fifo::fifo_pub::Dut])]
async fn a_clock_only<D: bench_fifo::Ports>(dut: D) {
    clock_only(dut).await;
}

#[simjig::test(timeout = "none", duts = [bench_fifo::fifo::Dut, bench_fifo::fifo_pub::Dut])]
async fn b_monitor_only<D: bench_fifo::Ports>(dut: D) {
    monitor_only(dut).await;
}

#[simjig::test(timeout = "none", duts = [bench_fifo::fifo::Dut, bench_fifo::fifo_pub::Dut])]
async fn c_full<D: bench_fifo::Ports>(dut: D) {
    full(dut).await;
}
