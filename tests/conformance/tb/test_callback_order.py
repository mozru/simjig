"""Callback order around combinators and edge triggers on one signal.

counter DUT, clk 10 ns, rises at 5, 15, 25, ... ns. `First`, `Combine` and `with_timeout`
run each operand in a helper task (`_wait` in `_concurrent_waiters.py`); these scenarios
pin where those helpers, and the caller, run relative to other tasks the same callback
wakes, including nested combinators, cancelled losers and spawned combinators.
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
    Lock,
    Timer,
    ValueChange,
    select,
    with_timeout,
)

from cocotb.queue import Queue
from conflog import Log


async def counter_start(dut, p):
    dut.clk.value = 0
    Clock(dut.clk, 10, unit="ns").start(start_high=False)
    p.write(dut, "rst", 1)
    p.write(dut, "en", 1)
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
    await p.wait(FallingEdge(dut.clk), "FallingEdge(clk)")
    p.write(dut, "rst", 0)


def seen_of(dut):
    def seen(probe):
        probe.read(dut, "clk", "count")

    return seen


async def observers(dut, log, seen):
    """f_task waits on the edge after the combinator registered, sets ev2; z_task waits."""
    ev2 = Event()
    pf = log.probe("f_task")
    pz = log.probe("z_task")

    async def f_task():
        await pf.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
        seen(pf)
        ev2.set()

    async def z_task():
        await pz.wait(ev2.wait(), "Event.wait()")
        seen(pz)

    return cocotb.start_soon(z_task()), cocotb.start_soon(f_task())


@cocotb.test()
async def loser_scheduled(dut):
    """A First loser woken by the same callback (via an Event set by an earlier waiter)."""
    log = Log("loser_scheduled")
    p = log.probe("main")
    seen = seen_of(dut)
    await counter_start(dut, p)  # 10 ns
    for rnd in range(2):  # 15, 25 ns
        ev = Event()
        pt = log.probe("t_task")

        async def t_task(pt=pt, ev=ev):
            await pt.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            seen(pt)
            ev.set()

        tt = cocotb.start_soon(t_task())
        await p.wait(Timer(1, unit="ns"), "Timer(1ns)")
        tz, tf = await observers(dut, log, seen)
        e = ev.wait()
        r = RisingEdge(dut.clk)
        trig = First(e, r) if rnd == 0 else First(r, e)
        res = await p.wait(trig, "First")
        p.note("first", "ev" if res is e else "rise" if res is r else "?")
        seen(p)
        await tt
        await tf
        await tz
    log.save()


@cocotb.test()
async def nested_combinators(dut):
    """First of a Combine, Combine of a First, First of a First."""
    log = Log("nested_combinators")
    p = log.probe("main")
    seen = seen_of(dut)
    await counter_start(dut, p)  # 10 ns
    for rnd in range(3):  # 15, 25, 35 ns
        await p.wait(Timer(1, unit="ns"), "Timer(1ns)")
        tz, tf = await observers(dut, log, seen)
        if rnd == 0:
            trig = First(Combine(RisingEdge(dut.clk), ValueChange(dut.clk)), Timer(100, unit="ns"))
        elif rnd == 1:
            trig = Combine(First(RisingEdge(dut.clk), Timer(100, unit="ns")), Timer(1, unit="ns"))
        else:
            trig = First(First(RisingEdge(dut.clk), Timer(100, unit="ns")), Timer(200, unit="ns"))
        await p.wait(trig, f"nested{rnd}")
        seen(p)
        await tf
        await tz
    log.save()


@cocotb.test()
async def timeout_combinators(dut):
    """with_timeout around a First and around a Combine."""
    log = Log("timeout_combinators")
    p = log.probe("main")
    seen = seen_of(dut)
    await counter_start(dut, p)  # 10 ns
    for rnd in range(2):  # 15, 25 ns
        await p.wait(Timer(1, unit="ns"), "Timer(1ns)")
        tz, tf = await observers(dut, log, seen)
        if rnd == 0:
            inner = First(RisingEdge(dut.clk), Timer(100, unit="ns"))
        else:
            inner = Combine(RisingEdge(dut.clk), ValueChange(dut.clk))
        await p.wait(with_timeout(inner, 200, "ns"), f"with_timeout{rnd}")
        seen(p)
        await tf
        await tz
    log.save()


@cocotb.test()
async def cycles_join(dut):
    """ClockCycles awaited from a ValueChange(clk) wake joins the RisingEdge group."""
    log = Log("cycles_join")
    p = log.probe("main")
    pv = log.probe("vc_task")
    pe = log.probe("edge_task")
    pc = log.probe("cnt_task")
    pz = log.probe("z_task")
    seen = seen_of(dut)
    ev = Event()
    await counter_start(dut, p)  # 10 ns

    async def vc_task():
        await pv.wait(ValueChange(dut.clk), "ValueChange(clk)")  # 15
        seen(pv)
        ev.set()
        await pv.wait(ClockCycles(dut.clk, 2), "ClockCycles(clk,2)")
        seen(pv)
        await pv.wait(ValueChange(dut.clk), "ValueChange(clk)")
        seen(pv)
        await pv.wait(ClockCycles(dut.clk, 2), "ClockCycles(clk,2)")
        seen(pv)

    async def edge_task():
        for _ in range(5):
            await pe.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            seen(pe)

    async def cnt_task():
        for _ in range(2):
            await pc.wait(ValueChange(dut.count), "ValueChange(count)")
            seen(pc)
            await pc.wait(ClockCycles(dut.clk, 1), "ClockCycles(clk,1)")
            seen(pc)

    async def z_task():
        await pz.wait(ev.wait(), "Event.wait()")
        seen(pz)

    ts = [cocotb.start_soon(f()) for f in (vc_task, edge_task, cnt_task, z_task)]
    for t in ts:
        await t
    log.save()


@cocotb.test()
async def rise_changed(dut):
    """One task alternates RisingEdge(clk) and ValueChange(clk) while others loop."""
    log = Log("rise_changed")
    p = log.probe("main")
    px = log.probe("x_task")
    py = log.probe("y_task")
    pw = log.probe("w_task")
    seen = seen_of(dut)
    await counter_start(dut, p)  # 10 ns
    kinds = [
        (RisingEdge, "RisingEdge(clk)"),
        (ValueChange, "ValueChange(clk)"),
        (ValueChange, "ValueChange(clk)"),
        (RisingEdge, "RisingEdge(clk)"),
        (ValueChange, "ValueChange(clk)"),
        (FallingEdge, "FallingEdge(clk)"),
        (RisingEdge, "RisingEdge(clk)"),
        (ValueChange, "ValueChange(clk)"),
    ]

    async def x_task():
        for k, label in kinds:
            await px.wait(k(dut.clk), label)
            seen(px)

    async def y_task():
        for _ in range(8):
            await py.wait(ValueChange(dut.clk), "ValueChange(clk)")
            seen(py)

    async def w_task():
        for _ in range(4):
            await pw.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            seen(pw)

    ts = [cocotb.start_soon(f()) for f in (x_task, y_task, w_task)]
    for t in ts:
        await t
    log.save()


@cocotb.test()
async def many_kinds(dut):
    """24 tasks on three edge kinds of clk, each cycling through the kinds."""
    log = Log("many_kinds")
    p = log.probe("main")
    pz = log.probe("z_task")
    ev = Event()
    await counter_start(dut, p)  # 10 ns
    kinds = [
        (RisingEdge, "RisingEdge(clk)"),
        (ValueChange, "ValueChange(clk)"),
        (FallingEdge, "FallingEdge(clk)"),
    ]

    def make(i):
        pi = log.probe(f"t{i:02}")

        async def t():
            for j in range(3):
                k, label = kinds[(i + j) % 3]
                await pi.wait(k(dut.clk), label)
                pi.read(dut, "count")
                if i == 4 and j == 0:
                    ev.set()

        return t

    async def z_task():
        await pz.wait(ev.wait(), "Event.wait()")
        pz.read(dut, "count")

    ts = [cocotb.start_soon(make(i)()) for i in range(24)]
    ts.append(cocotb.start_soon(z_task()))
    for t in ts:
        await t
    log.save()


@cocotb.test()
async def primed_walk(dut):
    """Groups primed during a walk: ValueChange(count) from a clk wake, joined later."""
    log = Log("primed_walk")
    p = log.probe("main")
    pa = log.probe("a_task")
    pb = log.probe("b_task")
    pc = log.probe("c_task")
    pd = log.probe("d_task")
    seen = seen_of(dut)
    await counter_start(dut, p)  # 10 ns

    async def a_task():
        await pa.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 15
        seen(pa)
        await pa.wait(ValueChange(dut.count), "ValueChange(count)")
        seen(pa)
        await pa.wait(ValueChange(dut.clk), "ValueChange(clk)")
        seen(pa)

    async def b_task():
        await pb.wait(ValueChange(dut.clk), "ValueChange(clk)")  # 15
        seen(pb)
        await pb.wait(ValueChange(dut.count), "ValueChange(count)")
        seen(pb)
        await pb.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
        seen(pb)

    async def c_task():
        await pc.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 15
        seen(pc)
        await pc.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
        seen(pc)
        await pc.wait(ValueChange(dut.clk), "ValueChange(clk)")
        seen(pc)

    async def d_task():
        await pd.wait(ValueChange(dut.count), "ValueChange(count)")  # 15, after eval
        seen(pd)
        await pd.wait(ValueChange(dut.clk), "ValueChange(clk)")
        seen(pd)
        await pd.wait(ValueChange(dut.count), "ValueChange(count)")
        seen(pd)

    ts = [cocotb.start_soon(f()) for f in (a_task, b_task, c_task, d_task)]
    for t in ts:
        await t
    log.save()


@cocotb.test()
async def spawned_combinators(dut):
    """Combinators in spawned tasks, resolved by an Event set during the edge callback."""
    log = Log("spawned_combinators")
    p = log.probe("main")
    seen = seen_of(dut)
    await counter_start(dut, p)  # 10 ns
    ev = Event()
    pt = log.probe("t_task")
    pa = log.probe("a_first")
    pb = log.probe("b_combine")
    pc = log.probe("c_timeout")
    pd = log.probe("d_plain")
    pe = log.probe("e_first2")

    async def t_task():
        await pt.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
        seen(pt)
        ev.set()

    async def a_first():
        await pa.wait(First(ev.wait(), Timer(100, unit="ns")), "First")
        seen(pa)

    async def b_combine():
        await pb.wait(Combine(ev.wait(), RisingEdge(dut.clk)), "Combine")
        seen(pb)

    async def c_timeout():
        await pc.wait(with_timeout(ev.wait(), 100, "ns"), "with_timeout")
        seen(pc)

    async def d_plain():
        await pd.wait(ev.wait(), "Event.wait()")
        seen(pd)

    async def e_first2():
        await pe.wait(First(RisingEdge(dut.clk), ev.wait()), "First")
        seen(pe)

    ts = [cocotb.start_soon(f()) for f in (t_task, a_first, b_combine, c_timeout, d_plain, e_first2)]
    await p.wait(Timer(1, unit="ns"), "Timer(1ns)")
    tz, tf = await observers(dut, log, seen)
    ts += [tz, tf]
    for t in ts:
        await t
    log.save()


@cocotb.test()
async def sync_losers(dut):
    """First losers that wait on an Event, a Lock and a Queue, cancelled while waiting."""
    log = Log("sync_losers")
    p = log.probe("main")
    pe = log.probe("e_task")
    ph = log.probe("holder")
    pw = log.probe("w_task")
    pg = log.probe("g_task")
    seen = seen_of(dut)
    ev = Event()
    lock = Lock()
    q = Queue()
    await counter_start(dut, p)  # 10 ns

    async def e_task():
        await pe.wait(ev.wait(), "Event.wait()")
        seen(pe)

    async def holder():
        await lock.acquire()  # 10 ns
        await ph.wait(Timer(17, unit="ns"), "Timer(17ns)")  # 27 ns
        seen(ph)
        lock.release()

    async def w_task():
        await pw.wait(lock.acquire(), "Lock.acquire()")  # 27 ns, after main's cancelled wait
        seen(pw)
        lock.release()

    async def g_task():
        item = await pg.wait(q.get(), "Queue.get()")  # 37 ns
        pg.note("item", str(item))
        seen(pg)

    # Event (15 ns): the edge wins; setting the Event afterwards wakes only e_task.
    te = cocotb.start_soon(e_task())
    th = cocotb.start_soon(holder())
    r = RisingEdge(dut.clk)
    res = await p.wait(First(ev.wait(), r), "First")
    p.note("first", "rise" if res is r else "ev")
    seen(p)
    ev.set()

    # Lock (25 ns): holder has it until 27 ns; main's acquire, first in line, loses to the
    # edge, so the lock goes to w_task.
    tw = cocotb.start_soon(w_task())
    r = RisingEdge(dut.clk)
    res = await p.wait(First(lock.acquire(), r), "First")
    p.note("first", "rise" if res is r else "lock")
    seen(p)
    await th
    await tw

    # Queue (35 ns): main's get, first in line, loses to the edge; the item put at 37 ns
    # goes to g_task.
    tg = cocotb.start_soon(g_task())
    # `select`: `First` takes triggers only, and `Queue.get()` is a coroutine.
    i, _ = await p.wait(select(RisingEdge(dut.clk), q.get()), "First")
    p.note("first", "rise" if i == 0 else "get")
    seen(p)
    await p.wait(Timer(2, unit="ns"), "Timer(2ns)")  # 37 ns
    q.put_nowait(7)
    await tg
    await te
    log.save()


@cocotb.test()
async def cancelled_nest(dut):
    """A First whose losing operand is a Combine still waiting: the Combine's helpers are
    cancelled when its helper runs, and the caller resumes after they finish."""
    log = Log("cancelled_nest")
    p = log.probe("main")
    seen = seen_of(dut)
    await counter_start(dut, p)  # 10 ns
    pf = log.probe("f_task")
    events = [Event() for _ in range(6)]

    async def f_task():
        await pf.wait(RisingEdge(dut.clk), "RisingEdge(clk)")  # 15 ns
        seen(pf)
        events[0].set()

    def chain(i):
        pz = log.probe(f"z{i + 1}")

        async def z():
            await pz.wait(events[i].wait(), "Event.wait()")
            seen(pz)
            events[i + 1].set()

        return z

    ts = [cocotb.start_soon(f_task())]
    ts += [cocotb.start_soon(chain(i)()) for i in range(5)]
    await p.wait(Timer(1, unit="ns"), "Timer(1ns)")  # 11 ns
    inner = Combine(ValueChange(dut.count), Timer(100, unit="ns"))
    await p.wait(First(inner, RisingEdge(dut.clk)), "First")  # 15 ns
    seen(p)
    for t in ts:
        await t
    log.save()
