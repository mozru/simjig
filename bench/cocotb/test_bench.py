"""The cocotb half of the simjig-vs-cocotb benchmark (bench/README.md).

The same testbench as bench/simjig/tests/bench.rs, statement for statement; keep the two
equal. One cocotb test per variant:
- clock_only (A): cocotb's Clock runs and the test awaits BENCH_CYCLES rising edges.
- monitor_only (B): A plus a monitor task reading six signals per rising edge.
- full (C): a reset, then a producer, a consumer and a monitor checking every edge
  against a reference model of the FIFO.

Drivers write at the falling edge, the monitor samples at the rising edge, where every
signal still holds its pre-edge value (tests/conformance/README.md). Triggers and handles
are looked up once, outside the loops: the fastest idiomatic form.

Each test times its own body and appends one JSON line to the file BENCH_OUT.
"""

import json
import os
import time
from collections import deque

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge

DEPTH = 16
PHASE = 24
RATES = (10, 50, 90)
PRODUCER_SEED = 0x12345678
CONSUMER_SEED = 0x9E3779B9
MASK = 0xFFFFFFFF


class XorShift32:
    """Marsaglia's xorshift32 (13, 17, 5), the same stream as the Rust testbench's."""

    __slots__ = ("x",)

    def __init__(self, seed):
        self.x = seed

    def next(self):
        x = self.x
        x ^= (x << 13) & MASK
        x ^= x >> 17
        x ^= (x << 5) & MASK
        self.x = x
        return x

    def rate(self):
        return RATES[self.next() % 3]


def cycles():
    return int(os.environ.get("BENCH_CYCLES", "10000"))


def report(variant, n, start, clock, **result):
    secs = round(time.perf_counter() - start, 6)
    line = {"flow": "cocotb", "dut": "fifo", "variant": variant, "cycles": n,
            "seconds": secs, "clock_impl": clock.impl, **result}  # fmt: skip
    with open(os.environ["BENCH_OUT"], "a") as f:
        f.write(json.dumps(line) + "\n")


def start_clock(dut):
    # With COCOTB_TRUST_INERTIAL_WRITES=1 (run.py sets it), cocotb 2.1's Clock picks
    # impl="gpi" (cocotb/clock.py): the clock toggles in C++ (GPI), not in a Python
    # coroutine; each result records `clock.impl`. It starts high, like simjig's.
    clock = Clock(dut.clk, 10, unit="ns")
    clock.start()
    return clock


@cocotb.test()
async def clock_only(dut):
    n = cycles()
    start = time.perf_counter()
    clock = start_clock(dut)
    rising = RisingEdge(dut.clk)
    for _ in range(n):
        await rising
    report("clock_only", n, start, clock, hash=0)


@cocotb.test()
async def monitor_only(dut):
    n = cycles()
    start = time.perf_counter()
    clock = start_clock(dut)
    rising = RisingEdge(dut.clk)
    push, pop, full, empty, count, rd = (
        dut.push, dut.pop, dut.full, dut.empty, dut.count, dut.rd_data)  # fmt: skip

    async def monitor():
        h = 0
        for _ in range(n):
            await rising
            s = (int(push.value) + 2 * int(pop.value) + 4 * int(full.value)
                 + 8 * int(empty.value) + 16 * int(count.value)
                 + 1024 * int(rd.value))  # fmt: skip
            h = (h * 31 + s) & MASK
        return h

    h = await cocotb.start_soon(monitor())
    report("monitor_only", n, start, clock, hash=h)


@cocotb.test()
async def full(dut):
    n = cycles()
    start = time.perf_counter()
    clk, rst, push, wr, pop = dut.clk, dut.rst, dut.push, dut.wr_data, dut.pop
    full_, empty, count, rd = dut.full, dut.empty, dut.count, dut.rd_data
    rising, falling = RisingEdge(clk), FallingEdge(clk)
    clock = start_clock(dut)
    # Reset: two edges with every input idle, released at a falling edge.
    rst.value = 1
    push.value = 0
    pop.value = 0
    await rising
    await rising
    await falling
    rst.value = 0

    async def producer():
        rng, rate = XorShift32(PRODUCER_SEED), 0
        for i in range(n):
            if i % PHASE == 0:
                rate = rng.rate()
            await falling
            push.value = int(rng.next() % 100 < rate)
            wr.value = rng.next() & 0xFF

    async def consumer():
        rng, rate = XorShift32(CONSUMER_SEED), 0
        for i in range(n):
            if i % PHASE == 0:
                rate = rng.rate()
            await falling
            pop.value = int(rng.next() % 100 < rate)

    async def monitor():
        # The reference model: the entries, and what the registered rd_data shows (the
        # entry the last pop removed; zero after reset).
        entries = deque()
        shown = pushes = pops = h = 0
        for _ in range(n):
            await rising
            p, q, w = int(push.value), int(pop.value), int(wr.value)
            length = len(entries)
            assert int(count.value) == length, "count"
            assert int(full_.value) == (length == DEPTH), "full"
            assert int(empty.value) == (length == 0), "empty"
            assert int(rd.value) == shown, "rd_data"
            do_pop = q and length > 0
            do_push = p and (length < DEPTH or do_pop)
            if do_pop:
                shown = entries.popleft()
                pops += 1
                h = (h * 31 + shown) & MASK
            if do_push:
                entries.append(w)
                pushes += 1
        return pushes, pops, h

    prod = cocotb.start_soon(producer())
    cons = cocotb.start_soon(consumer())
    mon = cocotb.start_soon(monitor())
    await prod
    await cons
    pushes, pops, h = await mon
    report("full", n, start, clock, pushes=pushes, pops=pops, hash=h)
