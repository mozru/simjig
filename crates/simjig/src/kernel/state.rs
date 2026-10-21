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
//! - While a DPI-C import implemented in Rust runs ([`enter_dpi_call`]), the model is in
//!   the middle of an eval: there is no settled value to read and no time at which a write
//!   or a wake could apply. Every entry point into the kernel ([`with_current`],
//!   [`try_current`]: signal access, triggers, `spawn`, `now()`) panics then, naming the
//!   import.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, ThreadId};

use super::cell::DirectTable;
use super::clock::ClockEntry;
use super::failure::{Failure, FailureKind, SourceLocation};
use super::models::Models;
use super::recorder::{Recorder, TraceKind, TraceRecord};
use super::signal::WriteLog;
use super::slab::Key;
use super::tasks::{ReadyQueue, TaskTable, UnrunDrop, gen16};
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
    /// A synchronous test ([`Sim::run_sync`](crate::Sim::run_sync)) is running: writes go
    /// to the model at once, and there are no tasks, triggers or regions.
    Sync,
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
    /// The models' direct ways to their narrow slots ([`DirectTable`]), outside the
    /// `RefCell`, for signal reads and a synchronous run's writes: the kernel owns `models`
    /// for as long as this copy, and never reads or writes during a model call.
    pub(crate) direct: DirectTable,
    pub(crate) now: Cell<SimTime>,
    pub(crate) phase: Cell<Phase>,
    pub(crate) tasks: RefCell<TaskTable>,
    pub(crate) ready: RefCell<ReadyQueue>,
    /// The task being polled or dropped, which owns writes, spawns and registrations.
    pub(crate) current: Cell<Option<Key>>,
    /// Whose code is running inside `current`'s poll: `current` itself, or the
    /// combinator helper whose operand is being polled (`combinator`).
    pub(crate) context: Cell<Option<Key>>,
    /// The helper this poll of `current` runs, if it was woken through a helper.
    pub(crate) route: Cell<Option<Key>>,
    pub(crate) main: Cell<Option<Key>>,
    pub(crate) main_done: Cell<bool>,
    /// A combinator is dropping a cancelled operand: a `Task` handle dropped now is part
    /// of that cancellation, not the testbench's `let _ = spawn(..)` (`tasks::cascading`).
    pub(crate) cancelling_operand: Cell<bool>,
    /// Handles dropped before their tasks ever ran, during the poll now running; settled
    /// when it ends (`tasks::settle_unrun_drops`).
    pub(crate) unrun_drops: RefCell<Vec<UnrunDrop>>,
    pub(crate) waits: RefCell<WaitTable>,
    pub(crate) log: RefCell<WriteLog>,
    /// Kernel clocks, in declaration order (the order coincident edges are logged in).
    pub(crate) clocks: RefCell<Vec<ClockEntry>>,
    /// Read only through [`Kernel::failed`] and [`Kernel::take_failure`], which keep
    /// `has_failed` equal to `failure.is_some()`.
    failure: RefCell<Option<Failure>>,
    /// Whether `failure` is set: asked after every task poll and eval, so a plain flag
    /// rather than a `RefCell` borrow.
    has_failed: Cell<bool>,
    pub(crate) recorder: Recorder,
    /// The counters of [`KernelStats`] that the kernel keeps (the others are counted when
    /// asked): one `Cell` each, so a count is one add, not a copy of the whole struct.
    steps: Cell<u64>,
    evals: Cell<u64>,
    polls: Cell<u64>,
    /// The run's time limit, for a synchronous run (`kernel::sync_clock`); an async run checks
    /// `Sim`'s own.
    pub(crate) time_limit: Cell<Option<SimTime>>,
    /// [`Models::version`] and [`Models::written`] when the last value-change walk began
    /// (`sched`); `(u64::MAX, u64::MAX)`, which neither counter reaches, before the first.
    pub(crate) walked: Cell<(u64, u64)>,
    /// The rising edges of a synchronous run's [`SyncClock`](crate::SyncClock) (its only
    /// one); `None` until it is made.
    pub(crate) sync_cycles: Cell<Option<u64>>,
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

/// What every kernel entry point looks up: the installed kernel's address (null while
/// none is) and the DPI-C import running on this thread, if any. In one thread-local with
/// no destructor, so a lookup is a single access with no lazy-initialisation state; the
/// kernel itself is owned by `CURRENT`.
///
/// Why a raw address (rule 5): finding the kernel through `CURRENT` (a `RefCell` around an
/// `Rc`, in a thread-local with a destructor) on every signal access and every await cost
/// as much as the access. The address is valid exactly while `CURRENT` holds the kernel:
/// [`install`] sets it after storing the `Rc`, and [`InstallGuard`] clears it before taking
/// the `Rc` out. Only shared references are made from it, as `CURRENT`'s borrow did.
struct Entry {
    kernel: Cell<*const Kernel>,
    dpi_call: Cell<Option<&'static str>>,
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<Kernel>>> = const { RefCell::new(None) };
    static ENTRY: Entry = const {
        Entry {
            kernel: Cell::new(ptr::null()),
            dpi_call: Cell::new(None),
        }
    };
    static NAMES: RefCell<NameTables> = const { RefCell::new(VecDeque::new()) };
}

/// Marks a DPI-C call to `import` in progress on this thread until dropped; see the
/// module docs.
#[derive(Debug)]
pub(crate) struct DpiCallGuard {
    previous: Option<&'static str>,
}

/// Starts a DPI-C call to `import`: kernel entry points refuse to run until the guard
/// drops (it restores the previous state, so nesting is harmless).
#[must_use = "the call ends when the guard drops"]
pub(crate) fn enter_dpi_call(import: &'static str) -> DpiCallGuard {
    DpiCallGuard {
        previous: ENTRY.with(|e| e.dpi_call.replace(Some(import))),
    }
}

impl Drop for DpiCallGuard {
    fn drop(&mut self) {
        ENTRY.with(|e| e.dpi_call.set(self.previous));
    }
}

/// The DPI-C import running on this thread, if one is.
pub(crate) fn in_dpi_call() -> Option<&'static str> {
    ENTRY.with(|e| e.dpi_call.get())
}

/// Panics if a DPI-C call is in progress; `what` names the attempted operation.
#[track_caller]
#[cold]
fn refuse_in_dpi_call(what: &str, import: &str) -> ! {
    panic!(
        "DPI-C import `{import}` used {what}: DPI functions run inside eval; they can't \
         access signals or await"
    );
}

/// Runs `f` on the installed kernel (if any) and the DPI-C import running (if any).
#[expect(unsafe_code, reason = "the installed kernel's address (`Entry`)")]
fn with_entry<R>(f: impl FnOnce(Option<&Kernel>, Option<&'static str>) -> R) -> R {
    let (kernel, dpi) = ENTRY.with(|e| (e.kernel.get(), e.dpi_call.get()));
    // SAFETY: a non-null address is the kernel `CURRENT` holds (see `Entry`). It stays
    // alive while `f` runs: only the `InstallGuard` of the `Sim::run` below us on the stack
    // releases it. The reference does not outlive `f`.
    f(unsafe { kernel.as_ref() }, dpi)
}

impl Kernel {
    #[expect(unsafe_code, reason = "the kernel's copy of the models' direct table")]
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
            waits: RefCell::new(WaitTable::new(sim_gen, count as usize)),
            log: RefCell::new(WriteLog::new(count as usize)),
            clocks: RefCell::new(Vec::new()),
            names,
            starts,
            widths,
            // SAFETY: `models` moves into this kernel next to the copy, and the kernel uses
            // the copy only between model calls (DPI-C entry is refused).
            direct: unsafe { models.share_direct() },
            models: RefCell::new(models),
            now: Cell::new(SimTime::ZERO),
            phase: Cell::new(Phase::Active),
            tasks: RefCell::new(TaskTable::default()),
            ready: RefCell::new(ReadyQueue::default()),
            current: Cell::new(None),
            context: Cell::new(None),
            route: Cell::new(None),
            main: Cell::new(None),
            main_done: Cell::new(false),
            cancelling_operand: Cell::new(false),
            unrun_drops: RefCell::new(Vec::new()),
            failure: RefCell::new(None),
            has_failed: Cell::new(false),
            recorder: Recorder::default(),
            steps: Cell::new(0),
            evals: Cell::new(0),
            polls: Cell::new(0),
            walked: Cell::new((u64::MAX, u64::MAX)),
            sync_cycles: Cell::new(None),
            time_limit: Cell::new(None),
        }
    }

    /// Records the start of a timestep (`Step`).
    #[inline]
    pub(crate) fn record_step(&self) {
        self.record(TraceKind::Step, None, 0, 0);
    }

    /// Records the start of the `ReadOnly` region.
    #[inline]
    pub(crate) fn record_read_only(&self) {
        self.record(TraceKind::ReadOnly, None, 0, 0);
    }

    /// Records eval number `delta` of the timestep.
    #[inline]
    pub(crate) fn record_eval(&self, delta: u32) {
        self.record(TraceKind::Eval, None, u64::from(delta), 0);
    }

    /// Records an event.
    #[inline]
    pub(crate) fn record(&self, kind: TraceKind, task: Option<Key>, arg: u64, value: u64) {
        let task = task.map_or(u64::MAX, |k| {
            u64::from(k.index) << 16 | u64::from(gen16(k.gen_))
        });
        self.recorder.record(TraceRecord {
            time: self.now.get(),
            kind,
            task,
            arg,
            value,
        });
    }

    /// Counts a timestep.
    #[inline]
    pub(crate) fn count_step(&self) {
        self.steps.set(self.steps.get() + 1);
    }

    /// Counts a model eval.
    #[inline]
    pub(crate) fn count_eval(&self) {
        self.evals.set(self.evals.get() + 1);
    }

    /// Counts a task poll.
    #[inline]
    pub(crate) fn count_poll(&self) {
        self.polls.set(self.polls.get() + 1);
    }

    /// The counters kept so far ([`KernelStats`] without the table sizes and records).
    pub(crate) fn counted(&self) -> KernelStats {
        KernelStats {
            steps: self.steps.get(),
            evals: self.evals.get(),
            polls: self.polls.get(),
            ..KernelStats::default()
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.has_failed.get()
    }

    /// The run's failure so far, if any.
    pub(crate) fn failure(&self) -> Option<Failure> {
        self.failure.borrow().clone()
    }

    /// Takes the run's failure, if any (the run is over).
    pub(crate) fn take_failure(&self) -> Option<Failure> {
        self.has_failed.set(false);
        self.failure.borrow_mut().take()
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
        self.fail_named(kind, task.map(|k| self.task_name(k)), location, message);
    }

    /// [`Kernel::fail`] for a task named earlier, which may no longer exist.
    pub(crate) fn fail_named(
        &self,
        kind: FailureKind,
        task: Option<String>,
        location: Option<&'static SourceLocation>,
        message: String,
    ) {
        if self.failed() || self.phase.get() == Phase::Ended {
            return;
        }
        self.record(TraceKind::Failure, None, 0, 0);
        self.has_failed.set(true);
        *self.failure.borrow_mut() = Some(Failure {
            kind,
            time: self.now.get(),
            precision: self.precision,
            task,
            location,
            message,
        });
    }

    /// Panics if a synchronous test is running: `what` needs the async scheduler.
    #[track_caller]
    pub(crate) fn refuse_in_sync(&self, what: &str) {
        if self.phase.get() == Phase::Sync {
            refuse_sync(what);
        }
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

/// Panics: `what` was used in a synchronous test, which has no scheduler.
#[track_caller]
#[cold]
pub(crate) fn refuse_sync(what: &str) -> ! {
    panic!(
        "{what} in a synchronous test (`#[simjig::test(sync)]`): it has no scheduler, tasks \
         or triggers; step the clock with `SyncClock` (or make the test async)"
    );
}

/// Runs `f` on the current kernel.
///
/// # Panics
/// If no simulation is running on this thread, or inside a DPI-C call; `what` names the
/// operation in the message.
#[track_caller]
pub(crate) fn with_current<R>(what: &str, f: impl FnOnce(&Kernel) -> R) -> R {
    match try_current(what, f) {
        Some(r) => r,
        None => panic!("{what} needs a running simulation; call it inside Sim::run"),
    }
}

/// Runs `f` on the current kernel, if one is installed.
///
/// # Panics
/// Inside a DPI-C call; `what` names the operation in the message.
#[track_caller]
pub(crate) fn try_current<R>(what: &str, f: impl FnOnce(&Kernel) -> R) -> Option<R> {
    let found = with_entry(|kernel, dpi| match dpi {
        Some(import) => Err(import),
        None => Ok(kernel.map(f)),
    });
    match found {
        Ok(r) => r,
        Err(import) => refuse_in_dpi_call(what, import),
    }
}

/// Runs `f` on the current kernel if it is simulation `sim_gen`; for `Drop` paths that
/// must do nothing once their simulation is gone.
pub(crate) fn with_sim<R>(sim_gen: u32, f: impl FnOnce(&Kernel) -> R) -> Option<R> {
    with_entry(|kernel, _| kernel.filter(|k| k.sim_gen == sim_gen).map(f))
}

/// The flat slot of `model`'s own `slot`, given each model's first flat slot.
#[inline]
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

/// The installed kernel, shared, if any.
pub(crate) fn current_kernel() -> Option<Rc<Kernel>> {
    CURRENT.with_borrow(Option::clone)
}

/// Installs `kernel` as current until the guard drops.
pub(crate) fn install(kernel: Rc<Kernel>) -> InstallGuard {
    // `try_borrow_mut`: a combinator polls its operands while it borrows the kernel, so a
    // nested run inside one finds the cell borrowed rather than set.
    CURRENT.with(|cell| {
        let installed = cell.try_borrow_mut().ok().filter(|c| c.is_none());
        let Some(mut c) = installed else {
            panic!("Sim::run cannot be nested inside another Sim::run")
        };
        let address = Rc::as_ptr(&kernel);
        *c = Some(kernel);
        ENTRY.with(|e| e.kernel.set(address));
    });
    InstallGuard(())
}

pub(crate) struct InstallGuard(());

impl Drop for InstallGuard {
    fn drop(&mut self) {
        // Clear the address before the kernel can go (`Entry`); take it out first so the
        // kernel is not dropped while the cell is borrowed.
        ENTRY.with(|e| e.kernel.set(ptr::null()));
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

#[cfg(test)]
mod tests {
    use super::super::fake::FakeModel;
    use super::super::signal::RawSig;
    use super::super::sim::Sim;
    use super::super::tasks::spawn;
    use super::super::trigger::{now, rising};
    use super::*;

    #[test]
    fn kernel_entry_points_refuse_to_run_inside_a_dpi_call() {
        type Case = fn(RawSig);
        let cases: [(&str, Case); 4] = [
            ("now", |_| {
                std::hint::black_box(now());
            }),
            ("spawn", |_| spawn("x", async {}).detach()),
            ("read", |clk| {
                std::hint::black_box(clk.read_u64());
            }),
            ("rising", |clk| drop(rising(clk))),
        ];
        for (name, case) in cases {
            let mut model = FakeModel::new(Precision::NS);
            model.signal("clk", 1);
            let mut sim = Sim::from_fake(model);
            let clk = sim.signal("clk").unwrap();
            let failure = sim
                .run(async move {
                    let _call = enter_dpi_call("dpi_tick");
                    case(clk);
                })
                .unwrap_err();
            assert!(
                failure.message.contains("DPI-C import `dpi_tick` used")
                    && failure.message.contains(
                        "DPI functions run inside eval; they can't access signals or await"
                    ),
                "{name}: {failure}"
            );
            assert_eq!(in_dpi_call(), None, "the guard ended the call");
        }
    }
}
