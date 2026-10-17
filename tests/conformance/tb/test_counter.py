"""Conformance scenarios on tests/duts/counter: Timer, ClockCycles, ValueChange, First/Combine.

10 ns clock started low: rising edges at 5, 15, 25, ... ns. Reset is released at the
10 ns falling edge with en = 1, so count reads 1 after the 15 ns edge, 2 after 25 ns, ...
"""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import (
    ClockCycles,
    Combine,
    FallingEdge,
    First,
    ReadOnly,
    RisingEdge,
    Timer,
)

from conflog import Log


async def start(dut, p):
    dut.clk.value = 0
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    p.write(dut, "rst", 1)
    p.write(dut, "en", 1)
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
    p.write(dut, "rst", 0)


@cocotb.test()
async def timer(dut):
    """Reads after Timers that land between edges and exactly on rising edges."""
    log = Log("timer")
    p = log.probe("main")
    await start(dut, p)  # 10 ns
    for delay in (3, 2, 7, 3, 10, 10):  # lands at 13, 15, 22, 25, 35, 45 ns
        await p.wait(Timer(delay, unit="ns"), f"Timer({delay}ns)")
        p.read(dut, "clk", "count")
        await p.wait(ReadOnly(), "ReadOnly")
        p.read(dut, "clk", "count")
    await p.wait(Timer(1, unit="step"), "Timer(1step)")
    p.read(dut, "clk", "count")
    log.save()


@cocotb.test()
async def clock_cycles(dut):
    """ClockCycles on rising and falling edges: wake time and the count seen."""
    log = Log("clock_cycles")
    p = log.probe("main")
    await start(dut, p)  # 10 ns
    await p.wait(ClockCycles(dut.clk, 3), "ClockCycles(clk,3)")
    p.read(dut, "clk", "count")
    await p.wait(ClockCycles(dut.clk, 2, rising=False), "ClockCycles(clk,2,falling)")
    p.read(dut, "clk", "count")
    await p.wait(ClockCycles(dut.clk, 1), "ClockCycles(clk,1)")
    p.read(dut, "clk", "count")
    log.save()


@cocotb.test()
async def value_change(dut):
    """ValueChange on a DUT-driven output and on a testbench-driven input."""
    log = Log("value_change")
    p = log.probe("main")
    w = log.probe("en_watch")
    await start(dut, p)  # 10 ns

    async def watch_en():
        for _ in range(2):
            await w.wait(dut.en.value_change, "ValueChange(en)")
            w.read(dut, "en", "count")

    watcher = cocotb.start_soon(watch_en())
    for _ in range(3):
        await p.wait(dut.count.value_change, "ValueChange(count)")
        p.read(dut, "clk", "count")
    # Stop counting at 30 ns (a falling edge) for two cycles, then restart.
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
    p.write(dut, "en", 0)
    await p.wait(Timer(20, unit="ns"), "Timer(20ns)")
    p.write(dut, "en", 1)
    await p.wait(dut.count.value_change, "ValueChange(count)")
    p.read(dut, "clk", "count")
    await watcher
    log.save()


@cocotb.test()
async def first_combine(dut):
    """First returns the winning trigger; Combine resumes when the last one fires."""
    log = Log("first_combine")
    p = log.probe("main")
    await start(dut, p)  # 10 ns
    edge = RisingEdge(dut.clk)
    t3 = Timer(3, unit="ns")
    won = await First(edge, t3)
    p.woke("First(RisingEdge(clk),Timer(3ns))")
    p.note("first", "Timer(3ns)" if won is t3 else "RisingEdge(clk)")
    p.read(dut, "clk", "count")  # 13 ns
    t8 = Timer(8, unit="ns")
    won = await First(RisingEdge(dut.clk), t8)
    p.woke("First(RisingEdge(clk),Timer(8ns))")
    p.note("first", "Timer(8ns)" if won is t8 else "RisingEdge(clk)")
    p.read(dut, "clk", "count")  # 15 ns
    # A Timer coincident with the edge: edge at 25 ns, timer at 15 + 10 = 25 ns.
    t10 = Timer(10, unit="ns")
    won = await First(RisingEdge(dut.clk), t10)
    p.woke("First(RisingEdge(clk),Timer(10ns))")
    p.note("first", "Timer(10ns)" if won is t10 else "RisingEdge(clk)")
    p.read(dut, "clk", "count")
    await p.wait(
        Combine(ClockCycles(dut.clk, 2), Timer(13, unit="ns")),
        "Combine(ClockCycles(clk,2),Timer(13ns))",
    )
    p.read(dut, "clk", "count")
    await p.wait(
        Combine(RisingEdge(dut.clk), Timer(1, unit="ns")),
        "Combine(RisingEdge(clk),Timer(1ns))",
    )
    p.read(dut, "clk", "count")
    log.save()
