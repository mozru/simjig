//! Swarm testing: random task programs over random clock sets. After every run, whatever
//! its outcome:
//! - the task table, wait table and wait lists are empty (everything was cancelled and
//!   every trigger deregistered);
//! - timesteps were strictly increasing;
//! - a failure is one of the kinds a program can legitimately cause, never a kernel
//!   invariant panic or a `RefCell` double borrow;
//! - running the same seed again gives the same outcome and the same trace hash.
//!
//! A failing seed is printed; rerun it alone with `SWARM_SEED=<n>`.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use simjig::{
    Clock, Event, FailureKind, FakeModel, Lock, NextTimeStep, Precision, Queue, RawSig, ReadOnly,
    ReadWrite, Sim, SimDuration, SimTime, Task, Timer, TraceKind, changed, falling, first, join,
    rising, spawn, with_timeout, yield_now,
};

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
        usize::try_from(self.below(n as u64)).unwrap()
    }
}

#[derive(Clone, Debug)]
enum Op {
    Write(usize, u64),
    Rising,
    Falling,
    Changed(usize),
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
    Panic,
}

fn program(rng: &mut Rng, depth: u32) -> Rc<[Op]> {
    let len = 1 + rng.below(8);
    (0..len).map(|_| op(rng, depth)).collect()
}

fn op(rng: &mut Rng, depth: u32) -> Op {
    let nested = depth < 2;
    match rng.below(if nested { 26 } else { 20 }) {
        0..=2 => Op::Write(rng.index(2), rng.below(4)),
        3..=4 => Op::Rising,
        5 => Op::Falling,
        6 => Op::Changed(rng.index(3)),
        7..=9 => Op::Timer(1 + rng.below(15)),
        10 => Op::ReadWrite,
        11 => Op::ReadOnly,
        12 => Op::NextTimeStep,
        13 => Op::Yield,
        14 => Op::SetEvent(rng.index(2)),
        15 => Op::WaitEvent(rng.index(2)),
        16 => Op::Locked(rng.index(2), 1 + rng.below(5)),
        17 => Op::Put(rng.below(100)),
        18 => Op::Get,
        19 => {
            if rng.below(40) == 0 {
                Op::Panic
            } else {
                Op::CancelChild
            }
        }
        20 | 21 => Op::Spawn(program(rng, depth + 1)),
        22 => Op::SpawnDetached(program(rng, depth + 1)),
        23 => Op::Timeout(program(rng, depth + 1), 1 + rng.below(30)),
        24 => Op::First(program(rng, depth + 1), program(rng, depth + 1)),
        _ => {
            if rng.below(2) == 0 {
                Op::Join(program(rng, depth + 1), program(rng, depth + 1))
            } else {
                Op::AwaitChild
            }
        }
    }
}

/// Everything a program can touch, created inside the run.
struct World {
    clk: RawSig,
    inputs: [RawSig; 2],
    watched: [RawSig; 3],
    events: [Event; 2],
    locks: [Lock; 2],
    queue: Queue<u64>,
}

type Boxed = Pin<Box<dyn Future<Output = ()>>>;

fn interpret(world: Rc<World>, ops: Rc<[Op]>, name: String) -> Boxed {
    Box::pin(async move {
        let mut children: Vec<Task<()>> = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            let w = Rc::clone(&world);
            let child_name = format!("{name}{i}");
            match op {
                Op::Write(s, v) => world.inputs[*s].write_u64(*v),
                Op::Rising => rising(world.clk).await,
                Op::Falling => falling(world.clk).await,
                Op::Changed(s) => changed(world.watched[*s]).await,
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
                Op::Spawn(p) => children.push(spawn(
                    &child_name,
                    interpret(w, Rc::clone(p), child_name.clone()),
                )),
                Op::SpawnDetached(p) => {
                    spawn(&child_name, interpret(w, Rc::clone(p), child_name.clone())).detach();
                }
                Op::CancelChild => drop(children.pop()),
                Op::AwaitChild => {
                    if let Some(c) = children.pop() {
                        c.await;
                    }
                }
                Op::Timeout(p, t) => {
                    with_timeout(
                        SimDuration::from_ticks(*t),
                        interpret(w, Rc::clone(p), child_name),
                    )
                    .await
                    .ok();
                }
                Op::First(a, b) => first!(
                    interpret(Rc::clone(&w), Rc::clone(a), format!("{child_name}a")),
                    interpret(w, Rc::clone(b), format!("{child_name}b")),
                ),
                Op::Join(a, b) => {
                    join!(
                        interpret(Rc::clone(&w), Rc::clone(a), format!("{child_name}a")),
                        interpret(w, Rc::clone(b), format!("{child_name}b")),
                    );
                }
                Op::Panic => panic!("swarm panic"),
            }
        }
        for child in children {
            child.await;
        }
    })
}

fn model(rng: &mut Rng) -> Sim {
    let mut m = FakeModel::new(Precision::NS);
    let clk = m.signal("clk", 1);
    let a = m.signal("a", 8);
    let b = m.signal("b", 8);
    let sum = m.signal("sum", 8);
    let q = m.signal("q", 8);
    m.comb(move |s| s.set(sum, s.get(a) + s.get(b)));
    m.on_rising(clk, move |s| s.set(q, s.get(a)));
    if rng.below(3) == 0 {
        m.at(SimTime(1 + rng.below(50)), move |s| s.set(q, 0x55));
    }
    let mut sim = Sim::from_fake(m);
    if rng.below(4) != 0 {
        let clk = sim.signal("clk").unwrap();
        let period = 2 * (1 + rng.below(6));
        let clock = Clock::new(clk, SimDuration::from_ticks(period))
            .first_rise_at(SimDuration::from_ticks(rng.below(period)));
        sim.add_clock(clock).unwrap();
    }
    sim.set_time_limit(SimTime(500));
    sim
}

/// Runs one seed; returns its outcome summary and trace hash, after checking invariants.
fn run_seed(seed: u64) -> (String, u64) {
    let mut rng = Rng::new(seed);
    let mut sim = model(&mut rng);
    let tasks: Vec<Rc<[Op]>> = (0..=rng.below(5)).map(|_| program(&mut rng, 0)).collect();
    let sig = |n| sim.signal(n).unwrap();
    let (clk, a, b, sum, q) = (sig("clk"), sig("a"), sig("b"), sig("sum"), sig("q"));
    let outcome = sim.run(async move {
        let world = Rc::new(World {
            clk,
            inputs: [a, b],
            watched: [a, sum, q],
            events: [Event::new("e0"), Event::new("e1")],
            locks: [Lock::new("l0"), Lock::new("l1")],
            queue: Queue::new("q", 2),
        });
        let handles: Vec<Task<()>> = tasks
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                spawn(
                    &format!("t{i}"),
                    interpret(Rc::clone(&world), p, format!("t{i}.")),
                )
            })
            .collect();
        for h in handles {
            h.await;
        }
    });
    let summary = match &outcome {
        Ok(()) => "ok".to_owned(),
        Err(f) => {
            let legit = match &f.kind {
                FailureKind::TaskPanic { message } => {
                    message == "swarm panic" || message.contains("awaited in the ReadOnly phase")
                }
                FailureKind::WriteConflict { .. }
                | FailureKind::WriteInReadOnly { .. }
                | FailureKind::DeltaLimit { .. }
                | FailureKind::Starved { .. }
                | FailureKind::Deadlock { .. }
                | FailureKind::TimeLimit { .. } => true,
                // A kind added later is unexpected until this test learns it.
                _ => false,
            };
            assert!(legit, "seed {seed}: unexpected failure: {f}");
            f.to_string()
        }
    };
    let stats = sim.stats();
    assert_eq!(
        (stats.live_tasks, stats.live_waiters, stats.live_wait_lists),
        (0, 0, 0),
        "seed {seed}: tables not empty after the run ({summary})"
    );
    let steps: Vec<u64> = sim
        .recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect();
    assert!(
        steps.windows(2).all(|w| w[0] < w[1]),
        "seed {seed}: time went backwards or repeated: {steps:?}"
    );
    (summary, sim.trace_hash())
}

#[test]
fn swarm_programs_keep_kernel_invariants_and_are_deterministic() {
    let seeds: Vec<u64> = match std::env::var("SWARM_SEED") {
        Ok(s) => vec![s.parse().expect("SWARM_SEED is a u64")],
        Err(_) => (0..if cfg!(miri) { 8 } else { 400 }).collect(),
    };
    let mut kinds = std::collections::BTreeMap::new();
    for seed in seeds {
        let first = run_seed(seed);
        let again = run_seed(seed);
        assert_eq!(first, again, "seed {seed}: two runs differ");
        let kind = first.0.split(' ').nth(1).unwrap_or("ok").to_owned();
        *kinds.entry(kind).or_insert(0u32) += 1;
    }
    // Keep the generator honest: it must reach both clean runs and failures.
    eprintln!("swarm outcomes: {kinds:?}");
    if !cfg!(miri) && std::env::var("SWARM_SEED").is_err() {
        assert!(kinds.contains_key("ok") && kinds.len() > 3, "{kinds:?}");
    }
}
