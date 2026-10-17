"""Conformance scenario on tests/duts/derived_clk: triggers on an internal divided clock.

10 ns clk started low (rising edges at 5, 15, ... ns). Reset is released at the 20 ns
falling edge, so div_clk first rises in the eval of the 25 ns clk edge, then every 20 ns.
"""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import ClockCycles, FallingEdge, ReadOnly, RisingEdge

from conflog import Log


@cocotb.test()
async def derived_edge(dut):
    """Wake order and values for RisingEdge(div_clk) next to RisingEdge(clk)."""
    log = Log("derived_edge")
    p = log.probe("main")
    f = log.probe("fast")
    dut.clk.value = 0
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    p.write(dut, "rst", 1)
    await p.wait(ClockCycles(dut.clk, 2), "ClockCycles(clk,2)")
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")  # 20 ns
    p.write(dut, "rst", 0)

    async def fast():
        for _ in range(6):
            await f.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            f.read(dut, "clk", "div_clk", "fast_count", "slow_count")

    t = cocotb.start_soon(fast())
    for _ in range(3):
        await p.wait(RisingEdge(dut.div_clk), "RisingEdge(div_clk)")
        p.read(dut, "clk", "div_clk", "fast_count", "slow_count")
        await p.wait(ReadOnly(), "ReadOnly")
        p.read(dut, "div_clk", "fast_count", "slow_count")
    await t
    log.save()
