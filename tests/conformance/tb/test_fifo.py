"""Conformance scenario on tests/duts/fifo (DEPTH=4): a valid/ready stream with back-pressure.

10 ns clock started low. The driver and the sink change their inputs on falling edges, so
every input is stable at the next rising edge, where the handshakes happen. Both decide
whether their beat moved from a ReadOnly sample taken before that edge. A monitor logs
both sides right after each rising edge.
"""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, ReadOnly, RisingEdge

from conflog import Log

DATA = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]
READY = [0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1]


@cocotb.test()
async def fifo_stream(dut):
    log = Log("fifo_stream")
    drv = log.probe("driver")
    snk = log.probe("sink")
    mon = log.probe("monitor")
    dut.clk.value = 0
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    drv.write(dut, "rst", 1)
    drv.write(dut, "in_valid", 0)
    drv.write(dut, "in_data", 0)
    drv.write(dut, "out_ready", 0)
    await drv.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
    await drv.wait(FallingEdge(dut.clk), "FallingEdge(clk)")  # 10 ns
    drv.write(dut, "rst", 0)

    async def driver():
        i = 0
        while i < len(DATA):
            drv.write(dut, "in_valid", 1)
            drv.write(dut, "in_data", DATA[i])
            await drv.wait(ReadOnly(), "ReadOnly")
            drv.read(dut, "in_ready")
            moved = bool(dut.in_ready.value)
            await drv.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
            i += moved
        drv.write(dut, "in_valid", 0)

    async def sink():
        for ready in READY:
            await snk.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
            snk.write(dut, "out_ready", ready)
            await snk.wait(ReadOnly(), "ReadOnly")
            if ready and dut.out_valid.value:
                snk.read(dut, "out_data")

    async def monitor():
        for _ in range(len(READY)):
            await mon.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            mon.read(dut, "in_valid", "in_ready", "out_valid", "out_ready", "level")

    m = cocotb.start_soon(monitor())
    s = cocotb.start_soon(sink())
    d = cocotb.start_soon(driver())
    await m
    await s
    await d
    log.save()
