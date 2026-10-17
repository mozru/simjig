"""Smoke for tests/duts/fatal: fire the task selected by FATAL_KIND at the 15 ns edge."""

import os

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge, Timer


@cocotb.test()
async def fire(dut):
    kind = int(os.environ["FATAL_KIND"])
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    dut.rst.value = 1
    dut.fire.value = 0
    dut.kind.value = kind
    await FallingEdge(dut.clk)  # 10 ns
    dut.rst.value = 0
    dut.fire.value = 1
    await RisingEdge(dut.clk)  # 15 ns: the selected task runs in this eval
    dut.fire.value = 0
    for _ in range(3):
        await RisingEdge(dut.clk)
    print(f"SMOKE survived alive={int(dut.alive.value)}")
    await Timer(1, unit="ns")
