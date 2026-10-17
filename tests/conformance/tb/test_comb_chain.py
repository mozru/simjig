"""Conformance scenario on tests/duts/comb_chain: when a combinational result is visible.

y = rotl1((a + 1) ^ 0x5a) + b, computed through four continuous assigns.
"""

import cocotb
from cocotb.triggers import ReadOnly, ReadWrite, Timer

from conflog import Log


@cocotb.test()
async def comb_settle(dut):
    """Write a and b, then read y: immediately, at ReadWrite, at ReadOnly, after a Timer."""
    log = Log("comb_settle")
    p = log.probe("main")
    w = log.probe("y_watch")

    async def watch_y():
        for _ in range(3):
            await w.wait(dut.y.value_change, "ValueChange(y)")
            w.read(dut, "a", "y")

    p.write(dut, "a", 0)
    p.write(dut, "b", 0)
    await p.wait(Timer(1, unit="ns"), "Timer(1ns)")
    p.read(dut, "a", "b", "y", "y_eq")
    watcher = cocotb.start_soon(watch_y())
    for a, b in ((0x10, 0), (0x20, 3), (0xFF, 0x40)):
        p.write(dut, "a", a)
        p.write(dut, "b", b)
        p.read(dut, "a", "y")
        await p.wait(ReadWrite(), "ReadWrite")
        p.read(dut, "a", "y")
        await p.wait(ReadOnly(), "ReadOnly")
        p.read(dut, "a", "b", "y", "y_eq", "s1", "s2", "s3")
        await p.wait(Timer(1, unit="ns"), "Timer(1ns)")
        p.read(dut, "a", "y")
    # A write issued in ReadWrite is applied at once; the result is visible in the same
    # ReadWrite phase only after the next eval.
    await p.wait(ReadWrite(), "ReadWrite")
    p.write(dut, "a", 0x01)
    p.read(dut, "a", "y")
    await p.wait(ReadOnly(), "ReadOnly")
    p.read(dut, "a", "y")
    await watcher
    log.save()
