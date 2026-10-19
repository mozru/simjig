//! The Marlin flow of the benchmark's mid-size design (`bench/rtl/mid_pipe.sv`;
//! `bench/README.md`, "Designs"): the same three variants as
//! `bench/simjig/tests/mid.rs`, with the same stimulus and checks, and the same loop as
//! `bench.rs` (see its module comment for how a Marlin testbench steps the model).
//!
//! The model is built by Marlin at run time, on first use, into `BENCH_ARTIFACTS`
//! (`bench.py build` does it once); `BENCH_MID_RTL` replaces `mid_pipe.sv`
//! (`bench.py bug`).
#![expect(
    clippy::disallowed_methods,
    reason = "a benchmark measures wall-clock time"
)]

mod common;

use std::collections::VecDeque;
use std::time::Instant;

use common::{CONSUMER_SEED, Driver, PRODUCER_SEED, cycles, manifest_path, report, rtl};
use marlin::verilator::VerilatorRuntime;
use marlin::verilog::prelude::*;

#[verilog(src = "rtl/mid_pipe_bench.sv", name = "mid_pipe_bench")]
struct Mid;

/// Entries in the FIFO (`mid_pipe`'s `DEPTH`).
const DEPTH: usize = 256;
/// Samples from an entry's pop to its `out_valid`: `mid_pipe`'s `STAGES` plus one.
const LATENCY: u64 = 128 + 1;

/// Marlin's runtime: the wrapper, the design (`BENCH_MID_RTL` replaces it) and its FIFO.
fn runtime() -> VerilatorRuntime {
    common::runtime(&[
        manifest_path("rtl/mid_pipe_bench.sv"),
        rtl("BENCH_MID_RTL", "../rtl/mid_pipe.sv"),
        manifest_path("../../examples/lib/rtl/sync_fifo.sv"),
    ])
}

fn rise(dut: &mut Mid<'_>) {
    dut.clk = 1;
    dut.eval();
}

fn fall(dut: &mut Mid<'_>) {
    dut.clk = 0;
    dut.eval();
}

/// The two halves of a 64-bit word, folded into one.
fn fold64(x: u64) -> u32 {
    (x as u32) ^ ((x >> 32) as u32)
}

/// (A) The clock alone: `n` rising edges and the falling edges between them.
#[test]
fn a_clock_only() {
    let rt = runtime();
    let mut dut: Mid = rt.create_model_simple().expect("model");
    let n = cycles();
    let start = Instant::now();
    dut.eval(); // time 0's first eval, before the clock's first edge
    for i in 0..n {
        if i > 0 {
            fall(&mut dut);
        }
        rise(&mut dut);
    }
    report("mid", "clock_only", n, start, r#""hash":0"#);
}

/// (B) A monitor reading seven signals per rising edge.
#[test]
fn b_monitor_only() {
    let rt = runtime();
    let mut dut: Mid = rt.create_model_simple().expect("model");
    let n = cycles();
    let start = Instant::now();
    let mut hash = 0u32;
    dut.eval(); // time 0's first eval, before the clock's first edge
    for i in 0..n {
        if i > 0 {
            fall(&mut dut);
        }
        let s = u32::from(dut.push)
            + 2 * u32::from(dut.pop)
            + 4 * u32::from(dut.full)
            + 8 * u32::from(dut.empty)
            + 16 * u32::from(dut.count)
            + 16384 * u32::from(dut.out_valid);
        hash = hash.wrapping_mul(31).wrapping_add(s);
        hash = hash.wrapping_mul(31).wrapping_add(fold64(dut.out_data));
        rise(&mut dut);
    }
    report(
        "mid",
        "monitor_only",
        n,
        start,
        &format!(r#""hash":{hash}"#),
    );
}

/// The full testbench's scoreboard and what it counted.
#[derive(Default)]
struct Scoreboard {
    entries: VecDeque<u64>,
    /// Entries popped and inside the pipeline: (the sample they are due at, the entry).
    pipe: VecDeque<(u64, u64)>,
    pushes: u64,
    pops: u64,
    /// A hash of every `out_crc`, in order.
    hash: u32,
}

impl Scoreboard {
    /// The monitor at sample `i` (just before rising edge `i`): checks the pre-edge
    /// outputs, then applies the edge's push and pop.
    fn sample(&mut self, dut: &Mid<'_>, i: u64) {
        let (p, q, w) = (dut.push == 1, dut.pop == 1, dut.wr_data);
        let len = self.entries.len();
        assert_eq!(usize::from(dut.count), len, "count");
        assert_eq!(dut.full == 1, len == DEPTH, "full");
        assert_eq!(dut.empty == 1, len == 0, "empty");
        let due = self.pipe.front().is_some_and(|&(at, _)| at == i);
        assert_eq!(dut.out_valid == 1, due, "out_valid");
        if due {
            let (_, want) = self.pipe.pop_front().expect("due");
            assert_eq!(dut.out_data, want, "out_data");
            self.hash = self.hash.wrapping_mul(31).wrapping_add(dut.out_crc);
        }
        let do_pop = q && len > 0;
        let do_push = p && (len < DEPTH || do_pop);
        if do_pop {
            let e = self.entries.pop_front().expect("len > 0");
            self.pipe.push_back((i + LATENCY, e));
            self.pops += 1;
        }
        if do_push {
            self.entries.push_back(w);
            self.pushes += 1;
        }
    }
}

/// (C) Reset, then each cycle: the monitor (with its scoreboard) at the rising edge, the
/// producer and the consumer at the falling edge.
#[test]
fn c_full() {
    let rt = runtime();
    let mut dut: Mid = rt.create_model_simple().expect("model");
    let n = cycles();
    let start = Instant::now();
    // Reset: two edges with every input idle, released at a falling edge.
    (dut.rst, dut.push, dut.pop) = (1, 0, 0);
    dut.eval(); // time 0's first eval, before the clock's first edge
    rise(&mut dut);
    fall(&mut dut);
    rise(&mut dut);
    fall(&mut dut);
    dut.rst = 0;

    let mut producer = Driver::new(PRODUCER_SEED);
    let mut consumer = Driver::new(CONSUMER_SEED);
    let mut sb = Scoreboard {
        entries: VecDeque::with_capacity(DEPTH),
        ..Scoreboard::default()
    };
    for i in 0..n {
        sb.sample(&dut, i);
        rise(&mut dut);
        dut.push = u8::from(producer.fire(i));
        dut.wr_data = producer.rng.next64();
        dut.pop = u8::from(consumer.fire(i));
        fall(&mut dut);
    }
    let result = format!(
        r#""pushes":{},"pops":{},"hash":{}"#,
        sb.pushes, sb.pops, sb.hash
    );
    report("mid", "full", n, start, &result);
}
