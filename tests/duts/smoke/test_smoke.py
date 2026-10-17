"""Behavioural smoke tests for the shared DUTs that the conformance goldens do not drive.

Each test checks the behaviour its DUT's README promises. run.py builds each DUT and runs
only the matching test (named after the DUT).
"""

import cocotb
from cocotb.clock import Clock
from cocotb.simtime import get_sim_time
from cocotb.triggers import ClockCycles, FallingEdge, ReadOnly, RisingEdge, Timer


async def clock_and_reset(dut, cycles=2):
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    dut.rst.value = 1
    await ClockCycles(dut.clk, cycles)
    await FallingEdge(dut.clk)
    dut.rst.value = 0


@cocotb.test()
async def inv(dut):
    for a in (0, 1, 0):
        dut.a.value = a
        await Timer(1, unit="ns")
        assert int(dut.y.value) == 1 - a


@cocotb.test()
async def wide(dut):
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    widths = {1: "1", 8: "8", 33: "33", 64: "64", 65: "65", 200: "200"}
    for pattern in (0, 0xA5A5_5A5A_DEAD_BEEF_0123_4567_89AB_CDEF_F0F0_0F0F_1234_5678_9ABC):
        await FallingEdge(dut.clk)
        for w, n in widths.items():
            getattr(dut, f"i{n}").value = pattern & ((1 << w) - 1)
        await RisingEdge(dut.clk)
        await ReadOnly()
        for w, n in widths.items():
            mask = (1 << w) - 1
            assert int(getattr(dut, f"o{n}").value) == ~pattern & mask, f"o{n}"
            assert len(getattr(dut, f"o{n}")) == w
        assert int(dut.q200.value) == pattern & ((1 << 200) - 1)


def beat(op, tag, payload):
    return (op << 16) | (tag << 12) | payload


@cocotb.test()
async def struct_pipe(dut):
    dut.in_valid.value = 0
    dut.in_beat.value = 0
    dut.operand.value = 0
    await clock_and_reset(dut)
    cases = [(1, 7, 5, 3, 8), (2, 2, 0xF0F, 0x0FF, 0xFF0), (3, 9, 0x00F, 0, 0xFF0), (0, 1, 42, 9, 42)]
    for op, tag, payload, operand, want in cases:
        dut.in_valid.value = 1
        dut.in_beat.value = beat(op, tag, payload)
        dut.operand.value = operand
        await RisingEdge(dut.clk)  # stage 1 captures the beat
        await FallingEdge(dut.clk)
        dut.in_valid.value = 0
        await RisingEdge(dut.clk)  # stage 2 applies op with the operand still held
        await ReadOnly()
        assert int(dut.out_valid.value) == 1
        assert int(dut.out_beat.value) == beat(op, tag, want), hex(int(dut.out_beat.value))
        await FallingEdge(dut.clk)


@cocotb.test()
async def timing_dut(dut):
    dut.start.value = 0
    await Timer(10, unit="ns")
    dut.start.value = 1
    await ReadOnly()
    assert int(dut.done.value) == 0
    await Timer(22, unit="ns")  # 32 ns: one ns before the 23 ns delay ends
    assert int(dut.done.value) == 0
    await Timer(1, unit="ns")  # 33 ns
    await ReadOnly()
    assert int(dut.done.value) == 1
    await Timer(17, unit="ns")  # 50 ns: iclk rose at 5, 15, 25, 35, 45
    assert int(dut.ticks.value) == 5
    await RisingEdge(dut.iclk_out)
    assert get_sim_time("ns") == 55


@cocotb.test()
async def dpi(dut):
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    for a, b in ((1, 2), (0xFFFF_FFFF, 1), (0x7FFF_FFFF, 0x7FFF_FFFF)):
        dut.a.value = a
        dut.b.value = b
        await Timer(1, unit="ns")
        assert int(dut.sum.value) == (a + b) & 0xFFFF_FFFF
    await ClockCycles(dut.clk, 4)
    await ReadOnly()
    assert int(dut.calls.value) == 4


@cocotb.test()
async def derived_clk(dut):
    await clock_and_reset(dut)
    await ClockCycles(dut.clk, 8)
    await ReadOnly()
    assert int(dut.fast_count.value) == 8
    assert int(dut.slow_count.value) == 4


@cocotb.test()
async def iface_top_wrap(dut):
    dut.in_valid.value = 0
    dut.in_data.value = 0
    dut.in_kind.value = 0
    dut.out_ready.value = 1
    await clock_and_reset(dut)
    dut.in_valid.value = 1
    dut.in_data.value = 0x1233
    dut.in_kind.value = 2
    await RisingEdge(dut.clk)
    await ReadOnly()
    assert int(dut.out_valid.value) == 1
    assert int(dut.out_data.value) == 0x1234
    assert int(dut.out_kind.value) == 2
