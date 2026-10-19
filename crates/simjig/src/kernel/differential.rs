//! The differential corpus: random task programs whose observable behaviour is pinned to
//! what the scheduler did before its fast paths (`docs/notes/sched.md`).
//!
//! Each seed builds a random simulation (one or two fake models, kernel clocks with random
//! periods, phases and duty cycles, and backends that do or do not settle, are held, and
//! report which slots an eval can change, as a Verilator model does) and random task
//! programs over it: edges of kernel clocks, of design-driven signals and of inputs,
//! `cycles`, edges followed by a region, Timers, regions, events, locks, queues, spawns,
//! cancellations, `first!`, `join!`, `with_timeout`, a combinator with a foreign waker,
//! clocks stopped and started, loops that re-arm one trigger, triggers dropped before or
//! after their first poll, and panics. Every resumption records the time, the task, the
//! step and every signal's value.
//!
//! A seed's digest covers its outcome (the failure report, with source lines blanked so
//! that editing this file does not move them), the trace hash (every eval, write, poll,
//! spawn and completion, in order), the kernel's counters and the resumption log. The
//! digests in `corpus.txt` were produced by the scheduler before the fast paths
//! (`SIMJIG_CORPUS_BLESS=1`); every later scheduler must reproduce them. Bless again only
//! for a deliberate change of semantics, which then needs its own review.
//!
//! A failing seed is printed; rerun it alone with `SIMJIG_CORPUS_SEED=<n>`.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use super::backend::{EvalStatus, ModelBackend, ModelError};
use super::clock::{Clock, ClockHandle};
use super::failure::FailureKind;
use super::fake::{FakeModel, FakeSig};
use super::signal::RawSig;
use super::sim::Sim;
use super::sync::{Event, Lock, Queue};
use super::tasks::{Task, spawn};
use super::time::{Precision, SimDuration, SimTime};
use super::trigger::{
    NextTimeStep, ReadOnly, ReadWrite, Timer, TriggerExt, changed, cycles, falling, now, rising,
    with_timeout, yield_now,
};
use crate::{first, join};

/// Seeds in the corpus; the corpus file has one digest per seed, in order.
const SEEDS: u64 = 4096;

/// xorshift64*: tiny, deterministic, good enough to pick operations.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn index(&mut self, n: usize) -> usize {
        usize::try_from(self.below(n as u64)).unwrap_or(0)
    }

    fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
}

/// FNV-1a 64, for the digests.
struct Fnv(u64);

impl Fnv {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
}

/// Kernel clocks: `clk0`, `clk1`, `clk2`.
const CLOCKS: usize = 3;

#[derive(Clone, Debug)]
enum Op {
    Write(usize, u64),
    WriteWide(u64),
    Rising(usize),
    Falling(usize),
    /// `changed` of one of [`World::watched`].
    Changed(usize),
    /// A rising edge of one of [`World::derived`] (design-driven 1-bit signals).
    DerivedRising(usize),
    Cycles(usize, u64),
    RisingThenReadWrite(usize),
    FallingThenReadOnly(usize),
    TimerThenReadWrite(u64),
    Timer(u64),
    ReadWrite,
    ReadOnly,
    NextTimeStep,
    Yield,
    SetEvent(usize),
    WaitEvent(usize),
    Locked(usize, u64),
    Put(u64),
    Get,
    Spawn(Rc<[Op]>),
    SpawnDetached(Rc<[Op]>),
    CancelChild,
    AwaitChild,
    Timeout(Rc<[Op]>, u64),
    First(Rc<[Op]>, Rc<[Op]>),
    Join(Rc<[Op]>, Rc<[Op]>),
    /// Both programs under a combinator of this file, which wakes through its own wakers.
    Foreign(Rc<[Op]>, Rc<[Op]>),
    /// The body, `n` times (a driver or monitor loop re-arming its triggers).
    Loop(u64, Rc<[Op]>),
    /// An edge trigger made and dropped without being polled.
    DropUnpolled(usize),
    /// An edge trigger polled once (registered) and dropped: `first!` of it and a yield.
    DropRegistered(usize),
    StopClock(usize),
    StartClock(usize),
    Panic,
}

fn program(rng: &mut Rng, depth: u32) -> Rc<[Op]> {
    let len = 1 + rng.below(7);
    (0..len).map(|_| op(rng, depth)).collect()
}

fn op(rng: &mut Rng, depth: u32) -> Op {
    let nested = depth < 2;
    let c = rng.index(CLOCKS);
    match rng.below(if nested { 40 } else { 30 }) {
        0..=2 => Op::Write(rng.index(3), rng.below(4)),
        3 => Op::WriteWide(rng.next()),
        4..=6 => Op::Rising(c),
        7 | 8 => Op::Falling(c),
        9 => Op::Changed(rng.index(5)),
        10 => Op::DerivedRising(rng.index(3)),
        11 => Op::Cycles(c, rng.below(4)),
        12 => Op::RisingThenReadWrite(c),
        13 => Op::FallingThenReadOnly(c),
        14 => Op::TimerThenReadWrite(1 + rng.below(9)),
        15 | 16 => Op::Timer(1 + rng.below(15)),
        17 => Op::ReadWrite,
        18 => Op::ReadOnly,
        19 => Op::NextTimeStep,
        20 => Op::Yield,
        21 => Op::SetEvent(rng.index(2)),
        22 => Op::WaitEvent(rng.index(2)),
        23 => Op::Locked(rng.index(2), 1 + rng.below(5)),
        24 => Op::Put(rng.below(100)),
        25 => Op::Get,
        26 => {
            if rng.one_in(30) {
                Op::Panic
            } else {
                Op::CancelChild
            }
        }
        27 => {
            if rng.one_in(2) {
                Op::DropUnpolled(c)
            } else {
                Op::DropRegistered(c)
            }
        }
        28 => {
            if rng.one_in(2) {
                Op::StopClock(c)
            } else {
                Op::StartClock(c)
            }
        }
        29 => Op::AwaitChild,
        30..=32 => Op::Spawn(program(rng, depth + 1)),
        33 => Op::SpawnDetached(program(rng, depth + 1)),
        34 => Op::Timeout(program(rng, depth + 1), 1 + rng.below(30)),
        35 => Op::First(program(rng, depth + 1), program(rng, depth + 1)),
        36 => Op::Join(program(rng, depth + 1), program(rng, depth + 1)),
        37 => Op::Foreign(program(rng, depth + 1), program(rng, depth + 1)),
        _ => Op::Loop(2 + rng.below(12), program(rng, depth + 1)),
    }
}

/// Everything a program can touch, created inside the run.
struct World {
    clocks: [RawSig; CLOCKS],
    handles: [ClockHandle; CLOCKS],
    inputs: [RawSig; 3],
    wide: RawSig,
    /// `changed` targets: an input, a clock, a comb output, a flop and a wide signal.
    watched: [RawSig; 5],
    /// Design-driven 1-bit signals: a divided clock, a flop bit, the second model's flop.
    derived: [RawSig; 3],
    /// Every signal, read at each resumption.
    all: Vec<RawSig>,
    events: [Event; 2],
    locks: [Lock; 2],
    queue: Queue<u64>,
    /// The resumption log, folded as it grows.
    log: RefCell<Fnv>,
    resumptions: Cell<u64>,
}

impl World {
    /// Records that `task` resumed after step `step`, with the time and every value.
    fn note(&self, task: &str, step: usize) {
        let mut log = self.log.borrow_mut();
        log.u64(now().ticks());
        log.bytes(task.as_bytes());
        log.u64(step as u64);
        for &sig in &self.all {
            let mut words = vec![0u32; sig.width().div_ceil(32) as usize];
            sig.read_words(&mut words);
            for w in words {
                log.u64(u64::from(w));
            }
        }
        self.resumptions.set(self.resumptions.get() + 1);
    }
}

type Boxed = Pin<Box<dyn Future<Output = ()>>>;

/// A waker that wakes the waker it wraps: a combinator of the testbench's own, which the
/// kernel cannot recognize as one of its tasks.
struct Forward(Mutex<Option<Waker>>);

impl Wake for Forward {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let waker = self.0.lock().map_or(None, |w| w.clone());
        if let Some(w) = waker {
            w.wake();
        }
    }
}

/// Polls both futures, each through its own [`Forward`] waker, until both are done.
struct Both {
    futures: [Option<Boxed>; 2],
    wakers: [Arc<Forward>; 2],
}

impl Future for Both {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self;
        for i in 0..2 {
            if let Ok(mut w) = this.wakers[i].0.lock() {
                *w = Some(cx.waker().clone());
            }
            let Some(f) = &mut this.futures[i] else {
                continue;
            };
            let waker = Waker::from(Arc::clone(&this.wakers[i]));
            if f.as_mut().poll(&mut Context::from_waker(&waker)).is_ready() {
                this.futures[i] = None;
            }
        }
        if this.futures.iter().all(Option::is_none) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

fn both(a: Boxed, b: Boxed) -> Both {
    Both {
        futures: [Some(a), Some(b)],
        wakers: [
            Arc::new(Forward(Mutex::new(None))),
            Arc::new(Forward(Mutex::new(None))),
        ],
    }
}

/// Runs one operation that involves no other program; returns `false` for the others.
async fn leaf(world: &World, op: &Op) -> bool {
    match op {
        Op::Write(s, v) => world.inputs[*s].write_u64(*v & 0xff),
        Op::WriteWide(v) => world.wide.write_words(&[
            u32::try_from(v & 0xffff_ffff).unwrap_or(0),
            u32::try_from(v >> 32).unwrap_or(0),
            u32::try_from(v & 0x3f).unwrap_or(0),
        ]),
        Op::Rising(c) => rising(world.clocks[*c]).await,
        Op::Falling(c) => falling(world.clocks[*c]).await,
        Op::Changed(s) => changed(world.watched[*s]).await,
        Op::DerivedRising(s) => rising(world.derived[*s]).await,
        Op::Cycles(c, n) => cycles(world.clocks[*c], *n).await,
        Op::RisingThenReadWrite(c) => rising(world.clocks[*c]).read_write().await,
        Op::FallingThenReadOnly(c) => falling(world.clocks[*c]).read_only().await,
        Op::TimerThenReadWrite(t) => Timer::new(SimDuration::from_ticks(*t)).read_write().await,
        Op::Timer(t) => Timer::new(SimDuration::from_ticks(*t)).await,
        Op::ReadWrite => ReadWrite::new().await,
        Op::ReadOnly => ReadOnly::new().await,
        Op::NextTimeStep => NextTimeStep::new().await,
        Op::Yield => yield_now().await,
        Op::SetEvent(e) => world.events[*e].set(),
        Op::WaitEvent(e) => world.events[*e].wait().await,
        Op::Locked(l, t) => {
            let _g = world.locks[*l].lock().await;
            Timer::new(SimDuration::from_ticks(*t)).await;
        }
        Op::Put(v) => world.queue.put(*v).await,
        Op::Get => {
            world.queue.get().await;
        }
        Op::DropUnpolled(c) => drop(rising(world.clocks[*c])),
        Op::DropRegistered(c) => first!(rising(world.clocks[*c]), yield_now()),
        Op::StopClock(c) => world.handles[*c].stop(),
        Op::StartClock(c) => {
            if !world.handles[*c].is_running() {
                world.handles[*c].start();
            }
        }
        Op::Panic => panic!("corpus panic"),
        _ => return false,
    }
    true
}

fn interpret(world: Rc<World>, ops: Rc<[Op]>, name: String) -> Boxed {
    Box::pin(async move {
        let mut children: Vec<Task<()>> = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            if !leaf(&world, op).await {
                let child = format!("{name}{i}");
                nest(&world, op, child, &mut children).await;
            }
            world.note(&name, i);
        }
        for child in children {
            child.await;
        }
    })
}

/// Runs one operation over other programs: spawns, cancellations, combinators, loops.
async fn nest(world: &Rc<World>, op: &Op, child: String, children: &mut Vec<Task<()>>) {
    let w = Rc::clone(world);
    match op {
        Op::Spawn(p) => children.push(spawn(&child, interpret(w, Rc::clone(p), child.clone()))),
        Op::SpawnDetached(p) => {
            spawn(&child, interpret(w, Rc::clone(p), child.clone())).detach();
        }
        Op::CancelChild => {
            if let Some(c) = children.pop() {
                c.cancel();
            }
        }
        Op::AwaitChild => {
            if let Some(c) = children.pop() {
                c.await;
            }
        }
        Op::Timeout(p, t) => {
            let program = interpret(w, Rc::clone(p), child);
            with_timeout(SimDuration::from_ticks(*t), program)
                .await
                .ok();
        }
        Op::First(a, b) => first!(
            interpret(Rc::clone(&w), Rc::clone(a), format!("{child}a")),
            interpret(w, Rc::clone(b), format!("{child}b")),
        ),
        Op::Join(a, b) => {
            join!(
                interpret(Rc::clone(&w), Rc::clone(a), format!("{child}a")),
                interpret(w, Rc::clone(b), format!("{child}b")),
            );
        }
        Op::Foreign(a, b) => {
            let a = interpret(Rc::clone(&w), Rc::clone(a), format!("{child}a"));
            both(a, interpret(w, Rc::clone(b), format!("{child}b"))).await;
        }
        Op::Loop(n, body) => {
            for round in 0..*n {
                interpret(Rc::clone(&w), Rc::clone(body), format!("{child}.{round}.")).await;
            }
        }
        _ => unreachable!("leaf operations are run by `leaf`"),
    }
}

/// A [`FakeModel`] that reports what a Verilator model reports: whether an eval with
/// nothing written since the last one is a no-op (`settles`), which slots an eval cannot
/// change (inputs), and whether it stays installed for the run (`sync_begin`).
struct Tuned {
    inner: FakeModel,
    settles: bool,
    hold: bool,
    /// Per slot: whether an eval may change it.
    eval_changes: Vec<bool>,
}

impl ModelBackend for Tuned {
    fn precision(&self) -> Precision {
        self.inner.precision()
    }
    fn slot_count(&self) -> u32 {
        self.inner.slot_count()
    }
    fn slot_name(&self, slot: u32) -> &str {
        self.inner.slot_name(slot)
    }
    fn slot_width(&self, slot: u32) -> u32 {
        self.inner.slot_width(slot)
    }
    fn read(&self, slot: u32, out: &mut [u32]) {
        self.inner.read(slot, out);
    }
    fn write(&mut self, slot: u32, value: &[u32]) {
        self.inner.write(slot, value);
    }
    fn set_time(&mut self, time: SimTime) {
        self.inner.set_time(time);
    }
    fn eval(&mut self) -> Result<EvalStatus, ModelError> {
        self.inner.eval()
    }
    fn eval_changes(&self, slot: u32) -> bool {
        self.eval_changes[slot as usize]
    }
    // The fake's logic is a function of its values, so an eval with nothing written since
    // the last one changes nothing, unless a timed event is due.
    fn settles(&self) -> bool {
        self.settles && !self.inner.events_pending()
    }
    fn sync_begin(&mut self) -> bool {
        self.hold
    }
    fn events_pending(&self) -> bool {
        self.inner.events_pending()
    }
    fn next_time_slot(&self) -> SimTime {
        self.inner.next_time_slot()
    }
}

fn tuned(rng: &mut Rng, inner: FakeModel, inputs: &[FakeSig]) -> Tuned {
    let tuned_inputs = rng.one_in(2);
    let eval_changes = (0..inner.slot_count())
        .map(|s| !(tuned_inputs && inputs.iter().any(|i| i.slot() == s)))
        .collect();
    Tuned {
        inner,
        settles: rng.one_in(2),
        hold: rng.one_in(2),
        eval_changes,
    }
}

/// The simulation of a seed: model 0 (three clocks, three 8-bit inputs and a wide one, a
/// sum, flops on two clocks and a divided clock), sometimes a second model, and clocks.
fn simulation(rng: &mut Rng) -> (Sim, bool) {
    let mut m = FakeModel::new(Precision::NS);
    let clks: Vec<FakeSig> = (0..CLOCKS)
        .map(|i| m.signal(&format!("clk{i}"), 1))
        .collect();
    let (a, b, en) = (m.signal("a", 8), m.signal("b", 8), m.signal("en", 8));
    let wide = m.signal("wide", 70);
    let sum = m.signal("sum", 8);
    let q = m.signal("q", 8);
    let cnt = m.signal("cnt", 8);
    let div = m.signal("div", 1);
    m.comb(move |s| s.set(sum, (s.get(a) + s.get(b)) & 0xff));
    m.on_rising(clks[0], move |s| s.set(q, s.get(a)));
    m.on_rising(clks[0], move |s| s.set(div, 1 - s.get(div)));
    m.on_rising(clks[1], move |s| {
        s.set(cnt, (s.get(cnt) + s.get(en)) & 0xff);
    });
    if rng.one_in(3) {
        m.at(SimTime(1 + rng.below(50)), move |s| s.set(q, 0x55));
    }
    let mut inputs = clks.clone();
    inputs.extend([a, b, en, wide]);
    let mut backends: Vec<Box<dyn ModelBackend>> = vec![Box::new(tuned(rng, m, &inputs))];
    let second = rng.one_in(4);
    if second {
        let mut m1 = FakeModel::new(Precision::NS);
        let mclk = m1.signal("m1clk", 1);
        let mq = m1.signal("m1q", 1);
        m1.on_rising(mclk, move |s| s.set(mq, 1 - s.get(mq)));
        backends.push(Box::new(tuned(rng, m1, &[mclk])));
    }
    let Ok(mut sim) = Sim::from_backends(backends) else {
        unreachable!("one precision")
    };
    sim.set_time_limit(SimTime(400));
    (sim, second)
}

/// How a kernel clock begins: added before the run, or declared inside it (started at
/// time 0, or stopped until a program starts it).
#[derive(Clone, Copy)]
enum ClockPlan {
    Added(ClockHandle),
    Declared {
        period: u64,
        phase: u64,
        high: Option<u64>,
        start: bool,
    },
}

/// A random plan for each kernel clock.
fn plan_clocks(rng: &mut Rng, sim: &mut Sim) -> [ClockPlan; CLOCKS] {
    std::array::from_fn(|i| {
        let period = 2 * (1 + rng.below(6));
        let phase = rng.below(period);
        let high = (period > 2 && rng.one_in(3)).then(|| 1 + rng.below(period - 1));
        let declared = ClockPlan::Declared {
            period,
            phase,
            high,
            start: rng.one_in(2),
        };
        if rng.one_in(3) {
            return declared;
        }
        let Some(sig) = sim.signal(&format!("clk{i}")) else {
            return declared;
        };
        let mut clock = Clock::new(sig, SimDuration::from_ticks(period))
            .first_rise_at(SimDuration::from_ticks(phase));
        if let Some(h) = high {
            clock = clock.high_for(SimDuration::from_ticks(h));
        }
        sim.add_clock(clock).map_or(declared, ClockPlan::Added)
    })
}

/// The handle of a planned clock, declaring (and starting) it if the plan says so.
fn clock_of(sig: RawSig, plan: ClockPlan) -> ClockHandle {
    match plan {
        ClockPlan::Added(h) => h,
        ClockPlan::Declared {
            period,
            phase,
            high,
            start,
        } => {
            let mut h = super::clock::clock(sig, SimDuration::from_ticks(period))
                .phase(SimDuration::from_ticks(phase));
            if let Some(high) = high {
                h = h.high_for(SimDuration::from_ticks(high));
            }
            if start { h.start() } else { h }
        }
    }
}

/// Blanks the digits after `.rs:` (source lines and columns), so that editing this file
/// does not change a failure's text.
fn without_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(".rs:") {
        out.push_str(&rest[..at + 4]);
        rest = rest[at + 4..].trim_start_matches(|c: char| c.is_ascii_digit() || c == ':');
    }
    out.push_str(rest);
    out
}

/// Whether a failure is one a program can legitimately cause.
fn legitimate(kind: &FailureKind) -> bool {
    match kind {
        FailureKind::TaskPanic { message } => {
            message == "corpus panic"
                || message.contains("awaited in the ReadOnly phase")
                || message.contains("started in the ReadOnly phase")
        }
        FailureKind::WriteConflict { .. }
        | FailureKind::WriteInReadOnly { .. }
        | FailureKind::DeltaLimit { .. }
        | FailureKind::ReadWriteLimit { .. }
        | FailureKind::Starved { .. }
        | FailureKind::Deadlock { .. }
        | FailureKind::TimeLimit { .. } => true,
        _ => false,
    }
}

/// The signals a program can touch, found by name.
struct Signals {
    clocks: [RawSig; CLOCKS],
    inputs: [RawSig; 3],
    wide: RawSig,
    watched: [RawSig; 5],
    derived: [RawSig; 3],
    all: Vec<RawSig>,
}

fn signals(sim: &Sim, second: bool) -> Signals {
    let names = [
        "clk0", "clk1", "clk2", "a", "b", "en", "wide", "sum", "q", "cnt", "div",
    ];
    let mut all: Vec<RawSig> = names.iter().filter_map(|n| sim.signal(n)).collect();
    let m1 = if second {
        all.extend(["m1clk", "m1q"].iter().filter_map(|n| sim.signal_in(1, n)));
        sim.signal_in(1, "m1q")
    } else {
        None
    };
    let sig = |n: &str| all[names.iter().position(|x| *x == n).unwrap_or(0)];
    Signals {
        clocks: [sig("clk0"), sig("clk1"), sig("clk2")],
        inputs: [sig("a"), sig("b"), sig("en")],
        wide: sig("wide"),
        watched: [sig("a"), sig("clk1"), sig("sum"), sig("q"), sig("wide")],
        derived: [sig("div"), sig("clk0"), m1.unwrap_or(sig("div"))],
        all,
    }
}

/// Runs `programs`, one task each; returns the run's outcome, the folded resumption log and
/// the number of resumptions.
fn run_programs(
    sim: &mut Sim,
    plans: [ClockPlan; CLOCKS],
    s: Signals,
    programs: Vec<Rc<[Op]>>,
) -> (Result<(), super::failure::Failure>, u64, u64) {
    let log = Rc::new(Cell::new((0u64, 0u64)));
    let log_out = Rc::clone(&log);
    let outcome = sim.run(async move {
        let world = Rc::new(World {
            handles: std::array::from_fn(|i| clock_of(s.clocks[i], plans[i])),
            clocks: s.clocks,
            inputs: s.inputs,
            wide: s.wide,
            watched: s.watched,
            derived: s.derived,
            all: s.all,
            events: [Event::new("e0"), Event::new("e1")],
            locks: [Lock::new("l0"), Lock::new("l1")],
            queue: Queue::new("q", 2),
            log: RefCell::new(Fnv::new()),
            resumptions: Cell::new(0),
        });
        let spawned: Vec<Task<()>> = programs
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                spawn(
                    &format!("t{i}"),
                    interpret(Rc::clone(&world), p, format!("t{i}.")),
                )
            })
            .collect();
        for h in spawned {
            h.await;
        }
        log.set((world.log.borrow().0, world.resumptions.get()));
    });
    let (folded, resumptions) = log_out.get();
    (outcome, folded, resumptions)
}

/// Runs one seed; returns its digest and a one-word outcome, after checking that the
/// kernel's tables are empty.
fn run_seed(seed: u64) -> (u64, String) {
    let mut rng = Rng::new(seed);
    let (mut sim, second) = simulation(&mut rng);
    let plans = plan_clocks(&mut rng, &mut sim);
    let programs: Vec<Rc<[Op]>> = (0..=rng.below(5)).map(|_| program(&mut rng, 0)).collect();
    let s = signals(&sim, second);
    let (outcome, folded, resumptions) = run_programs(&mut sim, plans, s, programs);
    let summary = match &outcome {
        Ok(()) => "ok".to_owned(),
        Err(f) => {
            assert!(legitimate(&f.kind), "seed {seed}: unexpected failure: {f}");
            without_lines(&f.to_string())
        }
    };
    let stats = sim.stats();
    assert_eq!(
        (stats.live_tasks, stats.live_waiters, stats.live_wait_lists),
        (0, 0, 0),
        "seed {seed}: tables not empty after the run ({summary})"
    );
    let mut digest = Fnv::new();
    digest.bytes(summary.as_bytes());
    for v in [
        sim.trace_hash(),
        stats.steps,
        stats.evals,
        stats.polls,
        stats.records,
        folded,
        resumptions,
    ] {
        digest.u64(v);
    }
    let word = summary.split(' ').nth(1).unwrap_or("ok").to_owned();
    (digest.0, word)
}

const CORPUS: &str = include_str!("differential/corpus.txt");

#[test]
fn the_scheduler_reproduces_the_corpus() {
    let bless = std::env::var_os("SIMJIG_CORPUS_BLESS").is_some();
    let seeds: Vec<u64> = match std::env::var("SIMJIG_CORPUS_SEED") {
        Ok(s) => vec![s.parse().unwrap_or(0)],
        Err(_) if cfg!(miri) => (0..6).collect(),
        Err(_) => (0..SEEDS).collect(),
    };
    let expected: Vec<&str> = CORPUS.lines().filter(|l| !l.starts_with('#')).collect();
    let mut kinds = std::collections::BTreeMap::new();
    let mut digests = Vec::new();
    let mut wrong = Vec::new();
    for &seed in &seeds {
        let (digest, word) = run_seed(seed);
        *kinds.entry(word).or_insert(0u32) += 1;
        let got = format!("{digest:016x}");
        if !bless
            && expected
                .get(usize::try_from(seed).unwrap_or(usize::MAX))
                .copied()
                != Some(got.as_str())
        {
            wrong.push(seed);
        }
        digests.push(got);
    }
    eprintln!("corpus outcomes: {kinds:?}");
    if bless {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/kernel/differential/corpus.txt"
        );
        let header = "# One digest per seed (kernel::differential); bless only for a reviewed \
                      change of semantics.\n";
        std::fs::write(path, header.to_owned() + &digests.join("\n") + "\n").unwrap_or(());
        return;
    }
    assert!(
        wrong.is_empty(),
        "{} of {} seeds differ from the corpus, first {:?}; rerun one with \
         SIMJIG_CORPUS_SEED=<n>",
        wrong.len(),
        seeds.len(),
        &wrong[..wrong.len().min(10)]
    );
    // Keep the generator honest: it must reach clean runs and several kinds of failure.
    if seeds.len() > 100 {
        assert!(kinds.contains_key("ok") && kinds.len() > 4, "{kinds:?}");
    }
}
