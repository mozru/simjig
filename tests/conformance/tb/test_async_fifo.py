"""Conformance scenarios on tests/duts/async_fifo (DEPTH=8): two clocks with coincident edges.

wclk has a 10 ns period and rclk a 30 ns period, both started low, so wclk rises at
5, 15, 25, ... ns and rclk at 15, 45, 75, ... ns: every rclk rising edge coincides with
a wclk rising edge.
"""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import ClockCycles, Combine, FallingEdge, ReadOnly, RisingEdge, Timer

from conflog import Log


def start_clocks(dut):
    dut.wclk.value = 0
    dut.rclk.value = 0
    Clock(dut.wclk, 10, unit="ns").start(start_high=False)
    Clock(dut.rclk, 30, unit="ns").start(start_high=False)


async def reset(dut, p):
    """Hold both resets for two rclk cycles; release them at the 60 ns falling edge."""
    p.write(dut, "wrst", 1)
    p.write(dut, "rrst", 1)
    p.write(dut, "winc", 0)
    p.write(dut, "wdata", 0)
    p.write(dut, "rinc", 0)
    await p.wait(ClockCycles(dut.rclk, 2), "ClockCycles(rclk,2)")
    await p.wait(FallingEdge(dut.rclk), "FallingEdge(rclk)")
    p.write(dut, "wrst", 0)
    p.write(dut, "rrst", 0)


@cocotb.test()
async def two_clocks(dut):
    """Wake order and values seen by tasks on two clocks whose rising edges coincide."""
    log = Log("two_clocks")
    p = log.probe("main")
    pw = log.probe("wclk_task")
    pr = log.probe("rclk_task")
    pc = log.probe("both_task")
    start_clocks(dut)
    await reset(dut, p)  # 60 ns

    async def on_wclk():
        for _ in range(7):
            await pw.wait(RisingEdge(dut.wclk), "RisingEdge(wclk)")
            pw.read(dut, "wclk", "rclk", "wbin", "rgray_w2", "wfull")

    async def on_rclk():
        for _ in range(3):
            await pr.wait(RisingEdge(dut.rclk), "RisingEdge(rclk)")
            pr.read(dut, "wclk", "rclk", "rbin", "wgray_r2", "rempty")

    async def on_both():
        await pc.wait(
            Combine(RisingEdge(dut.wclk), RisingEdge(dut.rclk)),
            "Combine(RisingEdge(wclk),RisingEdge(rclk))",
        )
        pc.read(dut, "wclk", "rclk", "wbin", "rbin")

    tw = cocotb.start_soon(on_wclk())
    tr = cocotb.start_soon(on_rclk())
    tc = cocotb.start_soon(on_both())
    # Write three words on wclk falling edges (70, 80, 90 ns), then stop.
    for value in (0xA1, 0xB2, 0xC3):
        await p.wait(FallingEdge(dut.wclk), "FallingEdge(wclk)")
        p.write(dut, "winc", 1)
        p.write(dut, "wdata", value)
    await p.wait(FallingEdge(dut.wclk), "FallingEdge(wclk)")
    p.write(dut, "winc", 0)
    await tw
    await tr
    await tc
    log.save()


@cocotb.test()
async def cdc_stream(dut):
    """Eight words cross from wclk to rclk; the reader pops on rclk falling edges."""
    log = Log("cdc_stream")
    pw = log.probe("writer")
    pr = log.probe("reader")
    start_clocks(dut)
    await reset(dut, pw)  # 60 ns
    data = [0x10 + i for i in range(8)]

    async def writer():
        i = 0
        while i < len(data):
            await pw.wait(FallingEdge(dut.wclk), "FallingEdge(wclk)")
            await pw.wait(ReadOnly(), "ReadOnly")
            pw.read(dut, "wfull")
            full = bool(dut.wfull.value)
            await pw.wait(Timer(1, unit="ns"), "Timer(1ns)")
            pw.write(dut, "winc", 0 if full else 1)
            pw.write(dut, "wdata", data[i])
            i += 0 if full else 1
        await pw.wait(FallingEdge(dut.wclk), "FallingEdge(wclk)")
        pw.write(dut, "winc", 0)

    async def reader():
        got = 0
        while got < len(data):
            await pr.wait(FallingEdge(dut.rclk), "FallingEdge(rclk)")
            await pr.wait(ReadOnly(), "ReadOnly")
            pr.read(dut, "rempty", "rdata")
            empty = bool(dut.rempty.value)
            await pr.wait(Timer(1, unit="ns"), "Timer(1ns)")
            pr.write(dut, "rinc", 0 if empty else 1)
            got += 0 if empty else 1
        await pr.wait(FallingEdge(dut.rclk), "FallingEdge(rclk)")
        pr.write(dut, "rinc", 0)

    w = cocotb.start_soon(writer())
    r = cocotb.start_soon(reader())
    await w
    await r
    log.save()
