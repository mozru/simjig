"""Where a task resumes after awaiting an Event, a Lock or a Queue, ready or not.

counter DUT, clk 10 ns, rises at 5, 15, 25, ... ns. In each round several tasks wake on
one `RisingEdge(clk)` and run in the order they registered; the lines show where the task
that awaits the primitive resumes among them.

- `Event.wait()` on a set Event and `Lock.acquire()` on a free Lock fire while the trigger
  is primed (`_Event._prime`, `Lock._prime_lock`), which schedules the task at the back of
  the queue: one hop. The Lock is taken at that moment, before the task resumes.
- `Queue.get()` on a non-empty queue and `Queue.put()` on a non-full one await nothing:
  no hop. A woken getter (putter) loops on `while self.empty()` (`full()`), so a task that
  runs before it takes the item (the slot), and the woken one waits again, last in line.
"""

import cocotb
from cocotb.queue import Queue
from cocotb.triggers import Event, First, Lock, RisingEdge, Timer
from conflog import Log
from test_callback_order import counter_start, seen_of


def edge_tasks(dut, log, bodies):
    """Starts one task per (name, body); each waits on RisingEdge(clk), then runs body."""

    def make(name, body):
        p = log.probe(name)

        async def task():
            await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
            await body(p)

        return task

    return [cocotb.start_soon(make(name, body)()) for name, body in bodies]


async def join_all(tasks):
    for t in tasks:
        await t


@cocotb.test()
async def event_ready(dut):
    """A set Event resumes its waiter one hop later, even if cleared meanwhile."""
    log = Log("event_ready")
    p = log.probe("main")
    seen = seen_of(dut)
    ev = Event()
    await counter_start(dut, p)  # 10 ns

    # 15 ns: a awaits the set Event and resumes after b and c; b clears it in between.
    ev.set()

    async def a(pa):
        await pa.wait(ev.wait(), "Event.wait()")
        seen(pa)

    async def b(pb):
        seen(pb)
        ev.clear()

    async def c(pc):
        seen(pc)

    await join_all(edge_tasks(dut, log, [("a", a), ("b", b), ("c", c)]))

    # 25 ns: w waits on the cleared Event; s sets it, and w resumes after t.
    pw = log.probe("w")

    async def w():
        await pw.wait(ev.wait(), "Event.wait()")
        seen(pw)

    async def s(ps):
        seen(ps)
        ev.set()

    async def t(pt):
        seen(pt)

    tw = cocotb.start_soon(w())
    await join_all([*edge_tasks(dut, log, [("s", s), ("t", t)]), tw])

    # 35 ns: the set Event inside a First, in a helper task that also takes a hop.
    async def d(pd):
        res = await pd.wait(First(ev.wait(), Timer(1, unit="ns")), "First")
        pd.note("first", "ev" if res is ev.wait() else "timer")
        seen(pd)

    async def e(pe):
        seen(pe)

    await join_all(edge_tasks(dut, log, [("d", d), ("e", e)]))
    log.save()


@cocotb.test()
async def lock_ready(dut):
    """A free Lock is taken at once, and its acquirer resumes one hop later."""
    log = Log("lock_ready")
    p = log.probe("main")
    seen = seen_of(dut)
    lock = Lock()
    await counter_start(dut, p)  # 10 ns

    # 15 ns: a takes the free lock and resumes after b and c; b sees it locked, c waits.
    # a releases at 17 ns; c resumes after the rest of a.
    async def a(pa):
        await pa.wait(lock.acquire(), "Lock.acquire()")
        seen(pa)
        await pa.wait(Timer(2, unit="ns"), "Timer(2ns)")
        lock.release()
        pa.note("locked", str(lock.locked()))

    async def b(pb):
        pb.note("locked", str(lock.locked()))
        seen(pb)

    async def c(pc):
        await pc.wait(lock.acquire(), "Lock.acquire()")
        seen(pc)
        lock.release()

    await join_all(edge_tasks(dut, log, [("a", a), ("b", b), ("c", c)]))

    # 25 ns: main holds the lock and releases it on the edge; w, waiting, resumes after
    # the other tasks the edge woke.
    await lock.acquire()
    pw = log.probe("w")

    async def w():
        await pw.wait(lock.acquire(), "Lock.acquire()")
        seen(pw)
        lock.release()

    async def r(pr):
        seen(pr)

    tw = cocotb.start_soon(w())
    ts = edge_tasks(dut, log, [("r", r)])
    await p.wait(RisingEdge(dut.clk), "RisingEdge(clk)")
    lock.release()
    seen(p)
    await join_all([*ts, tw])
    log.save()


@cocotb.test()
async def queue_get_ready(dut):
    """A get from a non-empty queue takes no hop, and takes the item from a woken getter."""
    log = Log("queue_get_ready")
    p = log.probe("main")
    seen = seen_of(dut)
    q = Queue()
    await counter_start(dut, p)  # 10 ns

    # 15 ns: the item is there; a resumes before b.
    q.put_nowait(5)

    async def a(pa):
        pa.note("item", str(await q.get()))
        seen(pa)

    async def b(pb):
        seen(pb)

    await join_all(edge_tasks(dut, log, [("a", a), ("b", b)]))

    # 25 ns: ga and gc wait; s puts 1, which wakes ga, but g runs first and takes it, so
    # ga waits again behind gc. The items put at 27 ns go to gc, then ga.
    def getter(name):
        pg = log.probe(name)

        async def task():
            item = await pg.wait(q.get(), "Queue.get()")
            pg.note("item", str(item))
            seen(pg)

        return task

    tga = cocotb.start_soon(getter("ga")())
    tgc = cocotb.start_soon(getter("gc")())

    async def s(ps):
        q.put_nowait(1)
        seen(ps)

    async def g(pg):
        pg.note("item", str(await q.get()))
        seen(pg)

    async def t(pt):
        seen(pt)

    ts = edge_tasks(dut, log, [("s", s), ("g", g), ("t", t)])
    await p.wait(Timer(12, unit="ns"), "Timer(12ns)")  # 27 ns
    q.put_nowait(2)
    q.put_nowait(3)
    await join_all([*ts, tga, tgc])
    log.save()


@cocotb.test()
async def queue_put_ready(dut):
    """A put into a non-full queue takes no hop, and takes the slot from a woken putter."""
    log = Log("queue_put_ready")
    p = log.probe("main")
    seen = seen_of(dut)
    q = Queue(maxsize=1)
    await counter_start(dut, p)  # 10 ns

    # 15 ns: there is room; a resumes before b. main empties the queue at 16 ns.
    async def a(pa):
        await q.put(1)
        pa.note("put", "1")
        seen(pa)

    async def b(pb):
        seen(pb)

    ts = edge_tasks(dut, log, [("a", a), ("b", b)])
    await p.wait(Timer(6, unit="ns"), "Timer(6ns)")  # 16 ns
    p.note("item", str(q.get_nowait()))
    await join_all(ts)

    # 25 ns: the queue is full; pa and pc wait. g takes the item, which wakes pa, but pb
    # runs first and fills the slot, so pa waits again behind pc. main empties the queue
    # at 27, 29 and 31 ns: 13, then 12 (pc), then 11 (pa).
    q.put_nowait(10)

    def putter(name, item):
        pp = log.probe(name)

        async def task():
            await pp.wait(q.put(item), "Queue.put()")
            pp.note("put", str(item))
            seen(pp)

        return task

    tpa = cocotb.start_soon(putter("pa", 11)())
    tpc = cocotb.start_soon(putter("pc", 12)())

    async def g(pg):
        pg.note("item", str(q.get_nowait()))
        seen(pg)

    async def pb(pp):
        await q.put(13)
        pp.note("put", "13")
        seen(pp)

    async def t(pt):
        seen(pt)

    ts = edge_tasks(dut, log, [("g", g), ("pb", pb), ("t", t)])
    await p.wait(Timer(11, unit="ns"), "Timer(11ns)")  # 27 ns
    for _ in range(3):
        p.note("item", str(q.get_nowait()))
        await p.wait(Timer(2, unit="ns"), "Timer(2ns)")
    await join_all([*ts, tpa, tpc])
    log.save()
