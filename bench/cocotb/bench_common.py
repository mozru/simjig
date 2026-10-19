"""What the cocotb testbenches of the benchmark (test_bench.py: the sync_fifo;
test_mid.py: the mid-size design) share: the stimulus stream, the run length, the clock
and the result line. The other flows have the same (bench/README.md, "Testbench").

A plain module, not a test module: it holds no cocotb tests, so importing it adds none.
"""

import json
import os
import time

from cocotb.clock import Clock

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

    def next64(self):
        """A 64-bit value from two draws, the high half first."""
        hi = self.next()
        return (hi << 32) | self.next()

    def rate(self):
        return RATES[self.next() % 3]


def cycles():
    return int(os.environ.get("BENCH_CYCLES", "10000"))


def report(variant, n, start, clock, dut, **result):
    secs = round(time.perf_counter() - start, 6)
    line = {"flow": "cocotb", "dut": dut, "variant": variant, "cycles": n,
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
