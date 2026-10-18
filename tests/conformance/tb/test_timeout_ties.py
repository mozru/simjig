"""Conformance scenario on tests/duts/flop: `with_timeout` when the timeout ties.

cocotb 2.1's `with_timeout(t, d)` is `select(Timer(d), t)`: the timeout's Timer is armed
before the awaited object, so when both are due in the same time step the timeout fires
first. The 10 ns clock starts low, so rising edges are at 5, 15, 25, ... ns.
"""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import (
    ClockCycles,
    FallingEdge,
    RisingEdge,
    SimTimeoutError,
    Timer,
    with_timeout,
)

from conflog import Log


async def wait_5ns():
    """A sub-coroutine whose own Timer is due exactly when the timeout is."""
    await Timer(5, unit="ns")



async def outcome(p, dut, label, awaitable, time, unit="ns"):
    """Await `with_timeout(awaitable, time, unit)` and log its outcome and the state."""
    try:
        await with_timeout(awaitable, time, unit)
        p.woke(label)
        p.note("outcome", "fired")
    except SimTimeoutError:
        p.woke(label)
        p.note("outcome", "SimTimeoutError")
    p.read(dut, "clk", "q")


@cocotb.test()
async def timeout_ties(dut):
    """Ties between the timeout and a Timer, a sub-coroutine, a task, an edge, ClockCycles."""
    log = Log("timeout_ties")
    p = log.probe("main")
    dut.clk.value = 0
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    p.write(dut, "rst", 1)
    p.write(dut, "d", 0)
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")  # 10 ns
    p.write(dut, "rst", 0)
    # 10 -> 15 ns: the awaited Timer and the timeout are both due at 15 ns.
    await outcome(p, dut, "with_timeout(Timer(5ns),5ns)", Timer(5, unit="ns"), 5)
    # 15 -> 20 ns: the same, inside a sub-coroutine.
    await outcome(p, dut, "with_timeout(wait_5ns(),5ns)", wait_5ns(), 5)
    # 20 -> 25 ns: the same, in a task started just before (it has not run yet, so its
    # Timer is armed after the timeout's).
    task = cocotb.start_soon(wait_5ns())
    await outcome(p, dut, "with_timeout(Task(wait_5ns()),5ns)", task, 5)
    # 30 -> 35 ns: the rising edge at 35 ns and the timeout.
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
    await outcome(p, dut, "with_timeout(RisingEdge(clk),5ns)", RisingEdge(dut.clk), 5)
    # 35 -> 45 ns: armed before the 35 ns edge, so the second cycle ends at 45 ns, with
    # the timeout.
    cycles = ClockCycles(dut.clk, 2)
    await outcome(p, dut, "with_timeout(ClockCycles(clk,2),10ns)", cycles, 10)
    # 45 -> 49.999 ns: one tick less than the timeout, and the Timer wins.
    await outcome(p, dut, "with_timeout(Timer(4999ps),5ns)", Timer(4999, unit="ps"), 5)
    # A task whose Timer was armed (at 49.999 ns) before the timeout's (at 50 ns): both are
    # due at 54.999 ns, the task's Timer fires first and runs it to completion, so its
    # result wins.
    task = cocotb.start_soon(wait_5ns())
    await p.wait(Timer(1, unit="step"), "Timer(1step)")
    await outcome(p, dut, "with_timeout(Task(wait_5ns()),4999ps)", task, 4999, "ps")
    log.save()
