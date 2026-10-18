//! The [`Kernel`] (all scheduler state of one simulation) and the thread-local *current
//! kernel* through which handles, triggers and wakers reach it.
//!
//! # Invariants
//! - The kernel is installed in the thread-local only while [`Sim::run`](crate::Sim::run)
//!   is on the stack; a guard uninstalls it, also during unwinding.
//! - Each piece of state is in its own `RefCell`, and no borrow is held while user code
//!   runs (polling or dropping a task future, waking a waker). That is what lets a task's
//!   `Drop` cleanup write signals or cancel other tasks from inside the kernel.
//! - Every simulation gets a process-wide unique `sim_gen`, so a handle or waker from
//!   another simulation is detected rather than silently aliased.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, ThreadId};

use super::clock::ClockEntry;
use super::failure::{Failure, FailureKind, SourceLocation};
use super::models::Models;
use super::recorder::{Recorder, TraceKind, TraceRecord};
use super::signal::WriteLog;
use super::slab::Key;
use super::tasks::{ReadyQueue, TaskTable, gen16};
use super::time::{Precision, SimTime};
use super::waits::WaitTable;

/// Scheduler phase within a timestep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Evals, edge wakes and task runs; writes are logged.
    Active,
    /// After the timestep settled; any write is a failure.
    ReadOnly,
    /// The run is over and tasks are being dropped; writes are discarded.
    Ended,
}

/// Counters exposed through [`Sim::stats`](crate::Sim::stats).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelStats {
    /// Distinct timesteps executed.
    pub steps: u64,
    /// Model evaluations.
    pub evals: u64,
    /// Task polls.
    pub polls: u64,
    /// Tasks still in the task table.
    pub live_tasks: usize,
    /// Trigger registrations still in the wait table.
    pub live_waiters: usize,
    /// Wait lists (one per task, event, lock and queue direction) still allocated.
    pub live_wait_lists: usize,
    /// Flight-recorder records written.
    pub records: u64,
}

pub(crate) struct Kernel {
    pub(crate) sim_gen: u32,
    pub(crate) precision: Precision,
    /// Flat slot names, shared with the retired-names registry for stale-handle messages.
    pub(crate) names: Rc<[Box<str>]>,
    /// Each model's first flat slot, then the total; also shared with the registry.
    pub(crate) starts: Rc<[u32]>,
    pub(crate) widths: Box<[u32]>,
    pub(crate) models: RefCell<Models>,
    pub(crate) now: Cell<SimTime>,
    pub(crate) phase: Cell<Phase>,
    pub(crate) tasks: RefCell<TaskTable>,
    pub(crate) ready: RefCell<ReadyQueue>,
    /// The task being polled or dropped, which owns writes, spawns and registrations.
    pub(crate) current: Cell<Option<Key>>,
    pub(crate) main: Cell<Option<Key>>,
    pub(crate) main_done: Cell<bool>,
    pub(crate) waits: RefCell<WaitTable>,
    pub(crate) log: RefCell<WriteLog>,
    /// Kernel clocks, in declaration order (the order coincident edges are logged in).
    pub(crate) clocks: RefCell<Vec<ClockEntry>>,
    pub(crate) failure: RefCell<Option<Failure>>,
    pub(crate) recorder: RefCell<Recorder>,
    pub(crate) counters: Cell<KernelStats>,
}

static NEXT_SIM_GEN: AtomicU32 = AtomicU32::new(1);

/// Simulations that exist, and the thread that owns each; consulted only when a waker
/// fires on a thread that is not running its simulation.
static LIVE_SIMS: Mutex<Vec<(u32, ThreadId)>> = Mutex::new(Vec::new());

/// Slot names of the most recent simulations on this thread, so a stale handle can still
/// name its signal after its simulation is gone.
const RETIRED_NAMES: usize = 64;

/// Slot-name tables (names, model starts) by simulation id.
type NameTables = VecDeque<(u32, Rc<[Box<str>]>, Rc<[u32]>)>;

thread_local! {
    static CURRENT: RefCell<Option<Rc<Kernel>>> = const { RefCell::new(None) };
    static NAMES: RefCell<NameTables> = const { RefCell::new(VecDeque::new()) };
}

impl Kernel {
    pub(crate) fn new(models: Models) -> Self {
        let sim_gen = NEXT_SIM_GEN.fetch_add(1, Ordering::Relaxed);
        let count = models.slot_count();
        let names: Rc<[Box<str>]> = (0..count).map(|s| models.slot_name(s).into()).collect();
        let starts: Rc<[u32]> = models.starts().into();
        let widths = (0..count).map(|s| models.slot_width(s)).collect();
        LIVE_SIMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((sim_gen, thread::current().id()));
        NAMES.with_borrow_mut(|n| {
            if n.len() == RETIRED_NAMES {
                n.pop_front();
            }
            n.push_back((sim_gen, Rc::clone(&names), Rc::clone(&starts)));
        });
        Self {
            sim_gen,
            precision: models.precision(),
            waits: RefCell::new(WaitTable::new(count as usize)),
            log: RefCell::new(WriteLog::new(count as usize)),
            clocks: RefCell::new(Vec::new()),
            names,
            starts,
            widths,
            models: RefCell::new(models),
            now: Cell::new(SimTime::ZERO),
            phase: Cell::new(Phase::Active),
            tasks: RefCell::new(TaskTable::default()),
            ready: RefCell::new(ReadyQueue::default()),
            current: Cell::new(None),
            main: Cell::new(None),
            main_done: Cell::new(false),
            failure: RefCell::new(None),
            recorder: RefCell::new(Recorder::default()),
            counters: Cell::new(KernelStats::default()),
        }
    }

    pub(crate) fn record(&self, kind: TraceKind, task: Option<Key>, arg: u64, value: u64) {
        let task = task.map_or(u64::MAX, |k| {
            u64::from(k.index) << 16 | u64::from(gen16(k.gen_))
        });
        self.recorder.borrow_mut().record(TraceRecord {
            time: self.now.get(),
            kind,
            task,
            arg,
            value,
        });
    }

    pub(crate) fn bump(&self, f: impl FnOnce(&mut KernelStats)) {
        let mut c = self.counters.get();
        f(&mut c);
        self.counters.set(c);
    }

    pub(crate) fn failed(&self) -> bool {
        self.failure.borrow().is_some()
    }

    /// Records a failure caused by `task`; only the first failure of a run is kept.
    pub(crate) fn fail(
        &self,
        kind: FailureKind,
        task: Option<Key>,
        location: Option<&'static SourceLocation>,
        message: String,
    ) {
        if self.failed() || self.phase.get() == Phase::Ended {
            return;
        }
        let task = task.map(|k| self.task_name(k));
        self.record(TraceKind::Failure, None, 0, 0);
        *self.failure.borrow_mut() = Some(Failure {
            kind,
            time: self.now.get(),
            precision: self.precision,
            task,
            location,
            message,
        });
    }

    /// The hierarchical name of a task, or a placeholder if it no longer exists.
    pub(crate) fn task_name(&self, key: Key) -> String {
        self.tasks
            .borrow()
            .name(key)
            .map_or_else(|| "<finished task>".to_owned(), |n| n.to_string())
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        LIVE_SIMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|&(g, _)| g != self.sim_gen);
    }
}

/// Runs `f` on the current kernel.
///
/// # Panics
/// If no simulation is running on this thread; `what` names the operation in the message.
#[track_caller]
pub(crate) fn with_current<R>(what: &str, f: impl FnOnce(&Kernel) -> R) -> R {
    CURRENT.with_borrow(|k| match k {
        Some(k) => f(k),
        None => panic!("{what} needs a running simulation; call it inside Sim::run"),
    })
}

/// Runs `f` on the current kernel, if one is installed.
pub(crate) fn try_current<R>(f: impl FnOnce(&Kernel) -> R) -> Option<R> {
    CURRENT
        .try_with(|c| c.borrow().as_deref().map(f))
        .ok()
        .flatten()
}

/// Runs `f` on the current kernel if it is simulation `sim_gen`; for `Drop` paths that
/// must do nothing once their simulation is gone.
pub(crate) fn with_sim<R>(sim_gen: u32, f: impl FnOnce(&Kernel) -> R) -> Option<R> {
    CURRENT
        .try_with(|c| match c.borrow().as_deref() {
            Some(k) if k.sim_gen == sim_gen => Some(f(k)),
            _ => None,
        })
        .ok()
        .flatten()
}

/// The flat slot of `model`'s own `slot`, given each model's first flat slot.
pub(crate) fn flat_slot(starts: &[u32], model: usize, slot: u32) -> Option<u32> {
    let (start, end) = (*starts.get(model)?, *starts.get(model + 1)?);
    let flat = start.checked_add(slot)?;
    (flat < end).then_some(flat)
}

/// The name of `model`'s `slot` in simulation `sim_gen`, if this thread still remembers it.
pub(crate) fn retired_name(sim_gen: u32, model: usize, slot: u32) -> Option<Box<str>> {
    NAMES.with_borrow(|n| {
        let (_, names, starts) = n.iter().find(|(g, _, _)| *g == sim_gen)?;
        let flat = flat_slot(starts, model, slot)?;
        names.get(flat as usize).cloned()
    })
}

/// Installs `kernel` as current until the guard drops.
pub(crate) fn install(kernel: Rc<Kernel>) -> InstallGuard {
    CURRENT.with_borrow_mut(|c| {
        assert!(
            c.is_none(),
            "Sim::run cannot be nested inside another Sim::run"
        );
        *c = Some(kernel);
    });
    InstallGuard(())
}

pub(crate) struct InstallGuard(());

impl Drop for InstallGuard {
    fn drop(&mut self) {
        // Take it out first so the kernel is not dropped while the cell is borrowed.
        let kernel = CURRENT.with_borrow_mut(Option::take);
        drop(kernel);
    }
}

/// The waker entry point: queues task `index`/`gen16` of simulation `sim_gen`.
///
/// A wake for a simulation that has ended, or a task that no longer exists, is a no-op.
///
/// # Panics
/// If the simulation is alive on another thread: the kernel is single-threaded, and a
/// waker sent across threads would race with it.
pub(crate) fn wake_task(sim_gen: u32, index: u32, gen16: u16) {
    let queued = with_sim(sim_gen, |k| k.ready.borrow_mut().push(index, gen16));
    if queued.is_some() {
        return;
    }
    let here = thread::current().id();
    let elsewhere = LIVE_SIMS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|&(g, t)| g == sim_gen && t != here);
    assert!(
        !elsewhere,
        "a simjig waker for simulation #{sim_gen} was woken from another thread; the \
         kernel is single-threaded, so keep futures and wakers on the thread running Sim::run"
    );
}
