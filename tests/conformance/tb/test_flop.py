"""Conformance scenarios on tests/duts/flop: edge reads, ReadOnly, write visibility, timeouts.

Every scenario uses a 10 ns clock started low, so rising edges are at 5, 15, 25, ... ns
and falling edges at 10, 20, 30, ... ns.
"""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import (
    FallingEdge,
    ReadOnly,
    ReadWrite,
    RisingEdge,
    SimTimeoutError,
    Timer,
    with_timeout,
)

from conflog import Log


def start_clock(dut):
    dut.clk.value = 0
    Clock(dut.clk, 10, unit="ns").start(start_high=False)


async def reset(dut, p):
    """Hold rst for one rising edge (5 ns), release it at the 10 ns falling edge."""
    p.write(dut, "rst", 1)
    p.write(dut, "d", 0)
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
    p.write(dut, "rst", 0)


@cocotb.test()
async def edge_read(dut):
    """Values seen right after RisingEdge and FallingEdge, with d driven on falling edges."""
    log = Log("edge_read")
    p = log.probe("main")
    p.read(dut, "clk", "rst", "d", "q")
    start_clock(dut)
    await reset(dut, p)
    for d in (1, 0, 1, 1, 0):
        p.write(dut, "d", d)
        await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
        p.read(dut, "clk", "d", "q")
        await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
        p.read(dut, "clk", "d", "q")
    log.save()


@cocotb.test()
async def readonly_read(dut):
    """Values seen in ReadOnly after a RisingEdge, compared with right after the edge."""
    log = Log("readonly_read")
    p = log.probe("main")
    start_clock(dut)
    await reset(dut, p)
    for d in (1, 0, 1):
        p.write(dut, "d", d)
        await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
        p.read(dut, "d", "q")
        await p.wait(ReadOnly(), "ReadOnly")
        p.read(dut, "clk", "d", "q")
        await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
    log.save()


@cocotb.test()
async def write_after_edge(dut):
    """A write right after RisingEdge: when d shows it, and which edge q captures it on."""
    log = Log("write_after_edge")
    p = log.probe("main")
    start_clock(dut)
    await reset(dut, p)
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 15 ns, q stays 0
    p.read(dut, "d", "q")
    p.write(dut, "d", 1)
    p.read(dut, "d", "q")  # same task, same delta, before yielding
    await p.wait(ReadWrite(), "ReadWrite")
    p.read(dut, "d", "q")
    await p.wait(ReadOnly(), "ReadOnly")
    p.read(dut, "d", "q")
    await p.wait(Timer(1, unit="ns"), "Timer(1ns)")
    p.read(dut, "d", "q")
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 25 ns, q captures 1
    p.read(dut, "d", "q")
    # Two writes to the same signal in one delta: only the last is applied.
    p.write(dut, "d", 0)
    p.write(dut, "d", 1)
    await p.wait(ReadOnly(), "ReadOnly")
    p.read(dut, "d", "q")
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 35 ns
    p.read(dut, "d", "q")
    log.save()


@cocotb.test()
async def timeout(dut):
    """with_timeout: a trigger that loses to the timeout, then one that beats it."""
    log = Log("timeout")
    p = log.probe("main")
    start_clock(dut)
    await reset(dut, p)  # now at 10 ns, next rising edge at 15 ns
    try:
        await with_timeout(RisingEdge(dut.clk), 3, "ns")
        p.woke("with_timeout(RisingEdge(clk),3ns)")
        p.note("outcome", "fired")
    except SimTimeoutError:
        p.woke("with_timeout(RisingEdge(clk),3ns)")
        p.note("outcome", "SimTimeoutError")
    p.read(dut, "clk", "q")
    # 13 ns now; the edge at 15 ns beats a 5 ns timeout (18 ns).
    try:
        await with_timeout(RisingEdge(dut.clk), 5, "ns")
        p.woke("with_timeout(RisingEdge(clk),5ns)")
        p.note("outcome", "fired")
    except SimTimeoutError:
        p.woke("with_timeout(RisingEdge(clk),5ns)")
        p.note("outcome", "SimTimeoutError")
    p.read(dut, "clk", "q")
    # Timeout exactly coincident with the edge: 15 ns + 10 ns = 25 ns, the next edge.
    try:
        await with_timeout(RisingEdge(dut.clk), 10, "ns")
        p.woke("with_timeout(RisingEdge(clk),10ns)")
        p.note("outcome", "fired")
    except SimTimeoutError:
        p.woke("with_timeout(RisingEdge(clk),10ns)")
        p.note("outcome", "SimTimeoutError")
    p.read(dut, "clk", "q")
    log.save()
