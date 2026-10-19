"""The cocotb flow of the benchmark's mid-size design (bench/rtl/mid_pipe.sv;
bench/README.md, "Designs").

The same testbench as bench/simjig/tests/mid.rs, statement for statement; keep the two
equal. The variants, the timing rule and the stimulus are test_bench.py's (the shared
parts are in bench_common.py). Full (C)'s monitor checks count, full and empty against
a model of the FIFO, and out_valid and out_data against the entries it saw popped, each
due LATENCY samples after its pop, and hashes every out_crc.
"""

import time
from collections import deque

import cocotb
from cocotb.triggers import FallingEdge, RisingEdge
from bench_common import (
    CONSUMER_SEED,
    MASK,
    PHASE,
    PRODUCER_SEED,
    XorShift32,
    cycles,
    report,
    start_clock,
)

DESIGN = "mid"
DEPTH = 256  # mid_pipe's DEPTH
LATENCY = 128 + 1  # samples from a pop to its out_valid: mid_pipe's STAGES + 1


def fold64(x):
    return (x ^ (x >> 32)) & MASK


@cocotb.test()
async def clock_only(dut):
    n = cycles()
    start = time.perf_counter()
    clock = start_clock(dut)
    rising = RisingEdge(dut.clk)
    for _ in range(n):
        await rising
    report("clock_only", n, start, clock, DESIGN, hash=0)


@cocotb.test()
async def monitor_only(dut):
    n = cycles()
    start = time.perf_counter()
    clock = start_clock(dut)
    rising = RisingEdge(dut.clk)
    push, pop, full, empty = dut.push, dut.pop, dut.full, dut.empty
    count, valid, data = dut.count, dut.out_valid, dut.out_data

    async def monitor():
        h = 0
        for _ in range(n):
            await rising
            s = (int(push.value) + 2 * int(pop.value) + 4 * int(full.value)
                 + 8 * int(empty.value) + 16 * int(count.value)
                 + 16384 * int(valid.value))  # fmt: skip
            h = (h * 31 + s) & MASK
            h = (h * 31 + fold64(int(data.value))) & MASK
        return h

    h = await cocotb.start_soon(monitor())
    report("monitor_only", n, start, clock, DESIGN, hash=h)


@cocotb.test()
async def full(dut):
    n = cycles()
    start = time.perf_counter()
    clk, rst, push, wr, pop = dut.clk, dut.rst, dut.push, dut.wr_data, dut.pop
    full_, empty, count = dut.full, dut.empty, dut.count
    valid, data, crc = dut.out_valid, dut.out_data, dut.out_crc
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
            wr.value = rng.next64()

    async def consumer():
        rng, rate = XorShift32(CONSUMER_SEED), 0
        for i in range(n):
            if i % PHASE == 0:
                rate = rng.rate()
            await falling
            pop.value = int(rng.next() % 100 < rate)

    async def monitor():
        # The scoreboard: the FIFO's entries, and the entries popped and inside the
        # pipeline, as (the sample they are due at, the entry).
        entries, pipe = deque(), deque()
        pushes = pops = h = 0
        for i in range(n):
            await rising
            p, q, w = int(push.value), int(pop.value), int(wr.value)
            length = len(entries)
            assert int(count.value) == length, "count"
            assert int(full_.value) == (length == DEPTH), "full"
            assert int(empty.value) == (length == 0), "empty"
            due = bool(pipe) and pipe[0][0] == i
            assert int(valid.value) == due, "out_valid"
            if due:
                assert int(data.value) == pipe.popleft()[1], "out_data"
                h = (h * 31 + int(crc.value)) & MASK
            do_pop = q and length > 0
            do_push = p and (length < DEPTH or do_pop)
            if do_pop:
                pipe.append((i + LATENCY, entries.popleft()))
                pops += 1
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
    report("full", n, start, clock, DESIGN, pushes=pushes, pops=pops, hash=h)
