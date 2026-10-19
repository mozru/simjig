//! The Marlin flow of the sync_fifo benchmark (`bench/README.md`): the same three variants
//! as `bench/simjig/tests/bench.rs` and `bench/cocotb/test_bench.py`, with the same
//! stimulus and checks, written the way Marlin is used: a `#[verilog]` struct whose ports
//! are plain fields, driven by a hand-written loop of `eval()` calls.
//!
//! Marlin has no scheduler: no tasks, no triggers, no delta cycles, no simulated time.
//! The testbench owns the loop, so "await a rising edge" becomes "set `clk = 1` and call
//! `eval()`", and the producer, consumer and monitor are three steps of one loop body, in
//! the order the event-driven flows run them:
//! - a cycle starts at the rising edge: the monitor samples first (every signal still
//!   holds its pre-edge value, as in the other flows: `tests/conformance/README.md`),
//!   then `clk = 1; eval()`;
//! - at the falling edge the producer and consumer write their inputs, then
//!   `clk = 0; eval()`.
//!
//! Time 0 is as in simjig's kernel: one eval with the initial inputs (outputs settle, so
//! the first sample sees `empty` high), then the clock's first edge, a rising one: the
//! clock starts high, like simjig's and cocotb's.
//!
//! What a Marlin `eval()` is: it passes every input field to the model (one FFI call per
//! input), calls Verilator's `eval` (one FFI call), and copies every output back into its
//! field (one FFI call per output); the model is a `dlopen`ed shared library. Reading a
//! signal is reading a field, so the monitor's reads of `push`, `pop` and `wr_data`
//! (inputs) read back what the testbench last wrote.
//!
//! The model is built by Marlin at run time, on first use, into `BENCH_ARTIFACTS`
//! (`bench.py build` does it once, with the same `-O2` as the other flows); `BENCH_RTL`
//! replaces the FIFO's RTL file (`bench.py bug`). Each test times its own body (model
//! creation excluded) and appends one JSON line to `BENCH_OUT` (stderr when unset).
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

#[verilog(src = "rtl/sync_fifo_bench.sv", name = "sync_fifo_bench")]
struct Fifo;

/// Entries in the FIFO (`rtl/sync_fifo_bench.sv`'s `DEPTH`).
const DEPTH: usize = 16;
/// Marlin's runtime: the wrapper plus the FIFO's RTL (`BENCH_RTL` replaces it).
fn runtime() -> VerilatorRuntime {
    let fifo = rtl("BENCH_RTL", "../../examples/lib/rtl/sync_fifo.sv");
    common::runtime(&[manifest_path("rtl/sync_fifo_bench.sv"), fifo])
}

fn rise(dut: &mut Fifo<'_>) {
    dut.clk = 1;
    dut.eval();
}

fn fall(dut: &mut Fifo<'_>) {
    dut.clk = 0;
    dut.eval();
}

/// (A) The clock alone: `n` rising edges and the falling edges between them.
#[test]
fn a_clock_only() {
    let rt = runtime();
    let mut dut: Fifo = rt.create_model_simple().expect("model");
    let n = cycles();
    let start = Instant::now();
    dut.eval(); // time 0's first eval, before the clock's first edge
    for i in 0..n {
        if i > 0 {
            fall(&mut dut);
        }
        rise(&mut dut);
    }
    report("fifo", "clock_only", n, start, r#""hash":0"#);
}

/// (B) A monitor reading six signals per rising edge.
#[test]
fn b_monitor_only() {
    let rt = runtime();
    let mut dut: Fifo = rt.create_model_simple().expect("model");
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
            + 1024 * u32::from(dut.rd_data);
        hash = hash.wrapping_mul(31).wrapping_add(s);
        rise(&mut dut);
    }
    report(
        "fifo",
        "monitor_only",
        n,
        start,
        &format!(r#""hash":{hash}"#),
    );
}

/// The full testbench's reference model and what it counted.
#[derive(Default)]
struct Model {
    entries: VecDeque<u32>,
    /// What the registered `rd_data` shows: the entry the last pop removed (zero after
    /// reset).
    shown: u32,
    pushes: u64,
    pops: u64,
    /// A hash of every popped byte, in order.
    hash: u32,
}

impl Model {
    /// The monitor at one rising edge: checks the pre-edge outputs, then applies the
    /// edge's push and pop.
    fn sample(&mut self, dut: &Fifo<'_>) {
        let (p, q, w) = (dut.push == 1, dut.pop == 1, u32::from(dut.wr_data));
        let len = self.entries.len();
        assert_eq!(usize::from(dut.count), len, "count");
        assert_eq!(dut.full == 1, len == DEPTH, "full");
        assert_eq!(dut.empty == 1, len == 0, "empty");
        assert_eq!(u32::from(dut.rd_data), self.shown, "rd_data");
        let do_pop = q && len > 0;
        let do_push = p && (len < DEPTH || do_pop);
        if do_pop {
            self.shown = self.entries.pop_front().expect("len > 0");
            self.pops += 1;
            self.hash = self.hash.wrapping_mul(31).wrapping_add(self.shown);
        }
        if do_push {
            self.entries.push_back(w);
            self.pushes += 1;
        }
    }
}

/// (C) Reset, then each cycle: the monitor (with its reference model) at the rising
/// edge, the producer and the consumer at the falling edge.
#[test]
fn c_full() {
    let rt = runtime();
    let mut dut: Fifo = rt.create_model_simple().expect("model");
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
    let mut model = Model {
        entries: VecDeque::with_capacity(DEPTH),
        ..Model::default()
    };
    for i in 0..n {
        model.sample(&dut);
        rise(&mut dut);
        dut.push = u8::from(producer.fire(i));
        dut.wr_data = (producer.rng.next() & 0xFF) as u8;
        dut.pop = u8::from(consumer.fire(i));
        fall(&mut dut);
    }
    let result = format!(
        r#""pushes":{},"pops":{},"hash":{}"#,
        model.pushes, model.pops, model.hash
    );
    report("fifo", "full", n, start, &result);
}
