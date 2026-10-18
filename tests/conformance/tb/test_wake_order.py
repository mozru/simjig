"""Conformance scenarios: the order in which one time step's callbacks wake tasks.

cocotb keeps one trigger per (signal, edge kind), and Verilator calls the value-change
callbacks behind them one at a time, in the order they were registered ("primed"), running
cocotb's whole event loop after each. So a task woken by one callback that then awaits a
trigger another task already waits on, whose callback has not run yet in this pass, joins
that callback and resumes in the same pass. `First`, `Combine` and `with_timeout` wait
through helper tasks, so their caller resumes after the tasks the same callback woke
directly. See tests/conformance/README.md, "Callback order".

Every clock starts low (`start_high=False`).
"""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import (
    ClockCycles,
    Combine,
    Event,
    FallingEdge,
    First,
    RisingEdge,
    Timer,
    ValueChange,
    with_timeout,
)

from conflog import Log


@cocotb.test()
async def coincident_join(dut):
    """async_fifo, wclk 10 ns and rclk 30 ns: rising edges coincide at 15, 45, 75 ns.

    15 ns: the rclk trigger was primed before the wclk one, so a task woken on rclk that
    awaits wclk joins the wclk callback still to come. 55 ns: nobody else waits on wclk
    at 45 ns, so the newly primed wclk trigger waits for the next edge. 85 ns: the wclk
    trigger was primed before the rclk one and has already fired at 75 ns.
    """
    log = Log("coincident_join")
    p = log.probe("main")
    pr = log.probe("r_task")
    pw = log.probe("w_task")
    pz = log.probe("z_task")
    pq = log.probe("q_task")
    ev = Event()
    dut.wclk.value = 0
    dut.rclk.value = 0
    Clock(dut.wclk, 10, unit="ns").start(start_high=False)
    Clock(dut.rclk, 30, unit="ns").start(start_high=False)
    p.write(dut, "winc", 0)

    def seen(probe):
        probe.read(dut, "wclk", "rclk", "winc")

    async def r():
        await pr.wait(RisingEdge(dut.rclk), "RisingEdge(rclk)")  # 15 ns
        seen(pr)
        pr.write(dut, "winc", 1)
        ev.set()
        await pr.wait(RisingEdge(dut.wclk), "RisingEdge(wclk)")  # 15 ns, joined
        seen(pr)
        await pr.wait(RisingEdge(dut.rclk), "RisingEdge(rclk)")  # 45 ns
        seen(pr)
        await pr.wait(RisingEdge(dut.wclk), "RisingEdge(wclk)")  # 55 ns
        seen(pr)
        await pr.wait(Timer(11, unit="ns"), "Timer(11ns)")  # 66 ns
        seen(pr)
        await pr.wait(RisingEdge(dut.rclk), "RisingEdge(rclk)")  # 75 ns
        seen(pr)
        await pr.wait(RisingEdge(dut.wclk), "RisingEdge(wclk)")  # 85 ns
        seen(pr)

    async def w():
        for _ in range(2):  # 15, 25 ns
            await pw.wait(RisingEdge(dut.wclk), "RisingEdge(wclk)")
            seen(pw)

    async def z():
        await pz.wait(ev.wait(), "Event.wait()")
        seen(pz)

    async def q():
        for _ in range(3):  # 65, 75, 85 ns
            await pq.wait(RisingEdge(dut.wclk), "RisingEdge(wclk)")
            seen(pq)

    tr = cocotb.start_soon(r())
    await p.wait(Timer(10, unit="ns"), "Timer(10ns)")
    tw = cocotb.start_soon(w())
    tz = cocotb.start_soon(z())
    await p.wait(Timer(50, unit="ns"), "Timer(50ns)")  # 60 ns
    tq = cocotb.start_soon(q())
    await tr
    await tw
    await tz
    await tq
    log.save()


async def join_on(dut, log, first, second, write):
    """Two signals change in one pass: `first`'s trigger primed before `second`'s.

    a_task waits on `first`, sets an Event, then waits on `second`, which b_task already
    waits on: it joins that callback (2 ns). At 4 ns nobody else waits on `second`, so
    a_task's wait on it lasts until the next change (6 ns).
    """
    p = log.probe("main")
    pa = log.probe("a_task")
    pb = log.probe("b_task")
    pz = log.probe("z_task")
    ev = Event()

    def seen(probe):
        probe.read(dut, "a", "b", "y", "y_eq")

    p.write(dut, "a", 0)
    p.write(dut, "b", 0)
    await p.wait(Timer(1, unit="ns"), "Timer(1ns)")

    async def a_task():
        for _ in range(2):
            await pa.wait(ValueChange(getattr(dut, first)), f"ValueChange({first})")
            seen(pa)
            ev.set()
            await pa.wait(ValueChange(getattr(dut, second)), f"ValueChange({second})")
            seen(pa)

    async def b_task():
        await pb.wait(ValueChange(getattr(dut, second)), f"ValueChange({second})")
        seen(pb)

    async def z_task():
        await pz.wait(ev.wait(), "Event.wait()")
        seen(pz)

    ta = cocotb.start_soon(a_task())
    tb = cocotb.start_soon(b_task())
    tz = cocotb.start_soon(z_task())
    for i, step in enumerate(write):  # at 2, 4, 6 ns
        delay = 1 if i == 0 else 2
        await p.wait(Timer(delay, unit="ns"), f"Timer({delay}ns)")
        for name, value in step:
            p.write(dut, name, value)
    await ta
    await tb
    await tz
    log.save()


@cocotb.test()
async def changed_join(dut):
    """comb_chain: a = 0x59 makes y 0 and y_eq 1 in one eval; a = 0 changes both back."""
    log = Log("changed_join")
    await join_on(dut, log, "y", "y_eq", [[("a", 0x59)], [("a", 0)], [("a", 0x59)]])


@cocotb.test()
async def written_join(dut):
    """comb_chain: the testbench writes a and b in one delta."""
    log = Log("written_join")
    await join_on(
        dut, log, "a", "b", [[("a", 1), ("b", 1)], [("a", 2), ("b", 2)], [("a", 3), ("b", 3)]]
    )


async def counter_start(dut, p):
    dut.clk.value = 0
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    p.write(dut, "rst", 1)
    p.write(dut, "en", 1)
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
    p.write(dut, "rst", 0)


@cocotb.test()
async def edge_kinds(dut):
    """counter: ValueChange(clk) and RisingEdge(clk) are separate callbacks.

    15 ns: ValueChange(clk) was primed first; its task then awaits RisingEdge(clk), which
    edge_task already waits on, and joins it. 35 ns: woken by ValueChange(clk) at 25 ns
    with nobody else on RisingEdge(clk), the newly primed RisingEdge waits for the next
    edge. 45 ns: ClockCycles(clk,1) joins the RisingEdge(clk) callback cycles_task primed
    at 41 ns.
    """
    log = Log("edge_kinds")
    p = log.probe("main")
    pv = log.probe("vc_task")
    pe = log.probe("edge_task")
    pz = log.probe("z_task")
    pc = log.probe("cycles_task")
    ev = Event()
    await counter_start(dut, p)  # 10 ns

    def seen(probe):
        probe.read(dut, "clk", "count")

    async def vc_task():
        await pv.wait(ValueChange(dut.clk), "ValueChange(clk)")  # 15 ns
        seen(pv)
        ev.set()
        await pv.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 15 ns, joined
        seen(pv)
        for _ in range(2):  # 20, 25 ns
            await pv.wait(ValueChange(dut.clk), "ValueChange(clk)")
            seen(pv)
        await pv.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 35 ns
        seen(pv)
        for _ in range(2):  # 40, 45 ns
            await pv.wait(ValueChange(dut.clk), "ValueChange(clk)")
            seen(pv)
        await pv.wait(ClockCycles(dut.clk, 1), "ClockCycles(clk,1)")  # 45 ns, joined
        seen(pv)

    async def edge_task():
        await pe.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 15 ns
        seen(pe)

    async def z_task():
        await pz.wait(ev.wait(), "Event.wait()")
        seen(pz)

    async def cycles_task():
        await pc.wait(ClockCycles(dut.clk, 2), "ClockCycles(clk,2)")  # 55 ns
        seen(pc)

    tv = cocotb.start_soon(vc_task())
    te = cocotb.start_soon(edge_task())
    tz = cocotb.start_soon(z_task())
    await p.wait(Timer(31, unit="ns"), "Timer(31ns)")  # 41 ns
    tc = cocotb.start_soon(cycles_task())
    await tv
    await te
    await tz
    await tc
    log.save()


@cocotb.test()
async def combinator_hop(dut):
    """counter: First, Combine and with_timeout resume their caller after the tasks the
    same callback woke directly.

    First and with_timeout cancel a still-pending loser, which costs one more hop than
    Combine: an Event set by a directly woken task wakes its waiter before the caller of
    First or with_timeout, and after the caller of Combine.
    """
    log = Log("combinator_hop")
    p = log.probe("main")
    pe = log.probe("e_task")
    await counter_start(dut, p)  # 10 ns

    def seen(probe):
        probe.read(dut, "clk", "count")

    async def e_task():
        for _ in range(3):  # 15, 25, 35 ns
            await pe.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            seen(pe)

    te = cocotb.start_soon(e_task())
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 15 ns
    seen(p)
    await p.wait(
        First(RisingEdge(dut.clk), Timer(100, unit="ns")), "First(RisingEdge(clk),Timer(100ns))"
    )  # 25 ns
    seen(p)
    await p.wait(
        Combine(RisingEdge(dut.clk), Timer(1, unit="ns")), "Combine(RisingEdge(clk),Timer(1ns))"
    )  # 35 ns
    seen(p)

    # One round per combinator, each at the next rising edge (45, 55, 65 ns): f_task waits
    # on the edge (registered after the combinator's own wait) and sets an Event that
    # z_task waits on.
    def first():
        return First(RisingEdge(dut.clk), Timer(100, unit="ns"))

    def combine():
        return Combine(RisingEdge(dut.clk), Timer(1, unit="ns"))

    def timeout():
        return with_timeout(RisingEdge(dut.clk), 100, "ns")

    rounds = (
        ("First(RisingEdge(clk),Timer(100ns))", first),
        ("Combine(RisingEdge(clk),Timer(1ns))", combine),
        ("with_timeout(RisingEdge(clk),100ns)", timeout),
    )
    for label, make in rounds:
        ev = Event()
        pf = log.probe("f_task")
        pz = log.probe("z_task")

        async def f_task(pf=pf, ev=ev):
            await pf.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            seen(pf)
            ev.set()

        async def z_task(pz=pz, ev=ev):
            await pz.wait(ev.wait(), "Event.wait()")
            seen(pz)

        tz = cocotb.start_soon(z_task())
        tf = cocotb.start_soon(f_task())
        await p.wait(make(), label)
        seen(p)
        await tf
        await tz
    await te
    log.save()
