//! Tasks: spawned futures with hierarchical names, owned by a handle or by their parent.
//!
//! # Ownership and cancellation (DECISION §1)
//! - [`spawn`] returns a [`Task<T>`] handle that owns the task: dropping the handle cancels
//!   the task. Cancellation is *only* dropping its future; cleanup is synchronous `Drop`,
//!   which may still write signals (through the write log) or release locks.
//! - Dropping a handle whose task never ran, and which was never awaited, fails the run
//!   (`FailureKind::Setup`, at the `spawn` call) when the poll that dropped it ends: that is
//!   `let _ = spawn(..)`, a driver that silently never runs. [`Task::cancel`] is the
//!   deliberate form. Drops that are a side effect of a larger cancellation are exempt (see
//!   `cascading`): a combinator cancelling an operand, a task being cancelled, the run
//!   ending, a panic unwinding; so are drops in the poll in which `main` returns, whose
//!   result (often the test's own error) ends the run (see `settle_unrun_drops`).
//! - [`Task::detach`] hands ownership to the task's nearest ancestor that is still running
//!   (its parent, unless the parent already completed). A task's detached children are
//!   cancelled when it completes or is cancelled, so no task outlives its scope, even when
//!   its handle was passed up and detached after its spawner finished. [`scope`] is a child task awaited in place; dropping the scope future cancels
//!   everything spawned inside it.
//! - Each poll and each drop runs under `catch_unwind`; a panic becomes a
//!   [`FailureKind::TaskPanic`] carrying the hierarchical task name, and stops the run.
//!
//! # Scheduling
//! The ready queue is FIFO and deduplicated: waking a queued task again does nothing, and a
//! wake for a task that no longer exists is dropped when it is popped.
//!
//! # Combinator helpers
//! The table also holds the helper tasks of `first!`, `join!` and `with_timeout` (one per
//! operand, `Helper`): queue places with a waker and a cancellation state but no future,
//! name or handle. Running one polls its root task routed to it; `combinator` explains the
//! protocol. They are not counted as tasks and never appear in failure reports.

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe, Location};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use super::OrInvariant;
use super::failure::FailureKind;
use super::panics;
use super::recorder::TraceKind;
use super::slab::{Key, Slab};
use super::state::{self, Kernel, Phase};
use super::waits::{ListKind, Registration, WaitOn};
use super::waker::{self, MAX_TASK_INDEX};

type BoxFuture = Pin<Box<dyn Future<Output = ()>>>;

/// The low 16 bits of a generation, as carried by a waker.
pub(crate) fn gen16(gen_: u32) -> u16 {
    u16::try_from(gen_ & 0xffff).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    /// Waiting to be polled; the future is in the table.
    Pending,
    /// Being polled; the future is on the poller's stack.
    Polling,
    /// The future completed; the result waits for the handle.
    Done,
    /// The future is being dropped.
    Cancelling,
}

struct TaskEntry {
    name: Rc<str>,
    /// Every ancestor, the root (`main`) first and the parent last. Keys of finished
    /// ancestors go stale, which the slab detects.
    ancestors: Rc<[Key]>,
    /// The task whose `detached` list holds this one, once detached.
    owner: Option<Key>,
    state: TaskState,
    future: Option<BoxFuture>,
    result: Option<Box<dyn Any>>,
    has_handle: bool,
    /// Whether the future was polled at least once.
    polled: bool,
    cancel_requested: bool,
    detached: Vec<Key>,
    /// Wait list the handle registers on to learn of completion.
    join: Key,
}

/// A combinator operand's helper task (cocotb's per-operand `Task` in `_wait`): a place in
/// the ready queue and a cancellation state, but no future of its own. Its operand lives in
/// the combinator, inside `root`'s future; running the helper polls `root` routed to it
/// (see `combinator`). Helpers have no name, no handle and no join list, and never appear
/// in failure reports.
struct Helper {
    /// The task whose future holds the operand.
    root: Key,
    /// What awaits the combinator: `root`, or the helper whose operand holds it.
    owner: Key,
    state: HelperState,
    /// Started combinators awaited inside this helper's operand that still have helpers
    /// running.
    live_forks: u32,
    /// Whether the helper's cancellation was passed on to those combinators.
    propagated: bool,
}

/// Where a helper is in cocotb's task life cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HelperState {
    /// Scheduled or waiting on a trigger.
    Running,
    /// `Task.cancel()` was called; it finishes when it next runs.
    Cancelling,
    /// Finished, normally or cancelled.
    Done,
}

enum Entry {
    Task(TaskEntry),
    Helper(Helper),
}

#[derive(Default)]
pub(crate) struct TaskTable {
    slab: Slab<Entry>,
    helpers: usize,
}

impl TaskTable {
    fn get(&self, key: Key) -> Option<&TaskEntry> {
        match self.slab.get(key)? {
            Entry::Task(e) => Some(e),
            Entry::Helper(_) => None,
        }
    }

    fn get_mut(&mut self, key: Key) -> Option<&mut TaskEntry> {
        match self.slab.get_mut(key)? {
            Entry::Task(e) => Some(e),
            Entry::Helper(_) => None,
        }
    }

    fn helper(&self, key: Key) -> Option<&Helper> {
        match self.slab.get(key)? {
            Entry::Helper(h) => Some(h),
            Entry::Task(_) => None,
        }
    }

    fn helper_mut(&mut self, key: Key) -> Option<&mut Helper> {
        match self.slab.get_mut(key)? {
            Entry::Helper(h) => Some(h),
            Entry::Task(_) => None,
        }
    }

    fn remove_task(&mut self, key: Key) -> Option<TaskEntry> {
        self.get(key)?;
        match self.slab.remove(key)? {
            Entry::Task(e) => Some(e),
            Entry::Helper(_) => None,
        }
    }

    fn tasks(&self) -> impl Iterator<Item = (Key, &TaskEntry)> {
        self.slab.iter().filter_map(|(k, e)| match e {
            Entry::Task(t) => Some((k, t)),
            Entry::Helper(_) => None,
        })
    }

    pub(crate) fn name(&self, key: Key) -> Option<Rc<str>> {
        self.get(key).map(|e| Rc::clone(&e.name))
    }

    /// Tasks in the table; helpers are not counted.
    pub(crate) fn len(&self) -> usize {
        self.slab.len() - self.helpers
    }

    /// Keys and names of tasks whose futures have not completed, in slot order.
    pub(crate) fn unfinished(&self) -> Vec<(Key, Rc<str>)> {
        self.tasks()
            .filter(|(_, e)| e.state != TaskState::Done)
            .map(|(k, e)| (k, Rc::clone(&e.name)))
            .collect()
    }

    /// How many combinator helpers of `root`'s combinators are in the table (finished or
    /// not): nonzero while `root` awaits a `join!`, `first!` or `with_timeout`.
    pub(crate) fn helpers_of(&self, root: Key) -> usize {
        self.slab
            .iter()
            .filter(|(_, e)| matches!(e, Entry::Helper(h) if h.root == root))
            .count()
    }

    pub(crate) fn keys(&self) -> Vec<Key> {
        self.tasks().map(|(k, _)| k).collect()
    }

    fn by_wake(&self, index: u32, gen16_: u16) -> Option<Key> {
        self.slab.key_at(index).filter(|k| gen16(k.gen_) == gen16_)
    }
}

impl fmt::Debug for TaskTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.tasks().map(|(_, e)| &e.name))
            .finish()
    }
}

/// FIFO queue of tasks to poll, deduplicated per task.
#[derive(Debug, Default)]
pub(crate) struct ReadyQueue {
    queue: VecDeque<(u32, u16)>,
    /// `queued[index]` is the generation queued for that slot, or `None`.
    queued: Vec<Option<u16>>,
}

impl ReadyQueue {
    /// Queues at the back (cocotb `EventLoop.schedule`); a no-op if already queued.
    pub(crate) fn push(&mut self, index: u32, gen16_: u16) {
        if self.mark(index, gen16_) {
            self.queue.push_back((index, gen16_));
        }
    }

    /// Queues at the front (cocotb `EventLoop.schedule_left`, used by `Task._start_next`);
    /// a no-op if already queued.
    fn push_front(&mut self, index: u32, gen16_: u16) {
        if self.mark(index, gen16_) {
            self.queue.push_front((index, gen16_));
        }
    }

    /// Records `index` as queued; returns `false` if it already was.
    fn mark(&mut self, index: u32, gen16_: u16) -> bool {
        let i = index as usize;
        if self.queued.len() <= i {
            self.queued.resize(i + 1, None);
        }
        if self.queued[i] == Some(gen16_) {
            return false;
        }
        self.queued[i] = Some(gen16_);
        true
    }

    fn pop(&mut self) -> Option<(u32, u16)> {
        let (index, g) = self.queue.pop_front()?;
        if self.queued[index as usize] == Some(g) {
            self.queued[index as usize] = None;
        }
        Some((index, g))
    }
}

/// Spawns `future` as a child of the current task, named `parent.name`.
///
/// The task is queued at once and first runs in the current delta, after the tasks
/// already queued. The returned handle owns it: await it for the result,
/// [`detach`](Task::detach) it, or [`cancel`](Task::cancel) it.
///
/// Nothing spawned in `main`'s last poll runs, detached or not: the run ends when `main`
/// returns, as cocotb kills a test's tasks when the test returns.
///
/// # Fire and forget
/// cocotb's `cocotb.start_soon(monitor())` keeps running on its own; here that is
/// `spawn(..).detach()`. Dropping the handle instead, as `let _ = spawn(..)` does,
/// cancels the task before it ever runs, so the run fails (`FailureKind::Setup`, pointing
/// at this call; not when `main` returns in that same poll, as with an early `return` or
/// `?`, since the run ends there) and records a
/// [`TraceKind::DroppedUnpolled`](crate::TraceKind::DroppedUnpolled). To cancel a task on
/// purpose, call [`Task::cancel`].
///
/// ```
/// use simjig::{FakeModel, Precision, Sim, Timer, SimDuration, spawn};
///
/// let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
/// let ticks = sim.run(async {
///     let count = std::rc::Rc::new(std::cell::Cell::new(0));
///     let seen = std::rc::Rc::clone(&count);
///     spawn("monitor", async move {
///         loop {
///             Timer::new(SimDuration::from_ticks(1)).await;
///             seen.set(seen.get() + 1);
///         }
///     })
///     .detach(); // runs until main returns
///     Timer::new(SimDuration::from_ticks(5)).await;
///     count.get()
/// });
/// assert_eq!(ticks.unwrap(), 4);
/// ```
///
/// # Panics
/// Outside [`Sim::run`](crate::Sim::run), or if 65 536 tasks are alive at once.
#[track_caller]
pub fn spawn<T: 'static>(name: &str, future: impl Future<Output = T> + 'static) -> Task<T> {
    let at = Location::caller();
    if state::with_current("spawn", |k| k.phase.get()) == Phase::Sync {
        state::refuse_sync("spawn");
    }
    state::with_current("spawn", |k| {
        let parent = k.current.get();
        spawn_in(k, parent, name, future, at)
    })
}

/// Spawns as [`spawn`] does; `at` is the call the handle's failures point at.
pub(crate) fn spawn_in<T: 'static>(
    k: &Kernel,
    parent: Option<Key>,
    name: &str,
    future: impl Future<Output = T> + 'static,
    at: &'static Location<'static>,
) -> Task<T> {
    let (full, ancestors): (Rc<str>, Rc<[Key]>) = {
        let tasks = k.tasks.borrow();
        match parent.and_then(|p| Some((p, tasks.get(p)?))) {
            Some((p, e)) => (
                format!("{}.{name}", e.name).into(),
                e.ancestors.iter().copied().chain([p]).collect(),
            ),
            None => (name.into(), Rc::new([])),
        }
    };
    let join = k
        .waits
        .borrow_mut()
        .alloc_list(ListKind::Task, Rc::clone(&full));
    let wrapped: BoxFuture = Box::pin(async move {
        let value = future.await;
        state::with_current("task completion", |k| {
            let key = k
                .current
                .get()
                .or_invariant("invariant: a completing task is current");
            if let Some(e) = k.tasks.borrow_mut().get_mut(key) {
                e.result = Some(Box::new(value));
            }
        });
    });
    let key = k.tasks.borrow_mut().slab.insert(Entry::Task(TaskEntry {
        name: Rc::clone(&full),
        ancestors,
        owner: None,
        state: TaskState::Pending,
        future: Some(wrapped),
        result: None,
        has_handle: true,
        polled: false,
        cancel_requested: false,
        detached: Vec::new(),
        join,
    }));
    assert!(
        key.index <= MAX_TASK_INDEX,
        "more than {} tasks alive at once (spawning `{full}`)",
        MAX_TASK_INDEX + 1
    );
    let parent_arg = parent.map_or(u64::MAX, |p| u64::from(p.index));
    k.record(TraceKind::Spawn, Some(key), parent_arg, 0);
    k.ready.borrow_mut().push(key.index, gen16(key.gen_));
    Task {
        sim_gen: k.sim_gen,
        key: Some(key),
        name: full,
        join: None,
        spawned_at: at,
        _output: PhantomData,
    }
}

/// Runs `future` as a child task named `name` and waits for it.
///
/// Everything spawned inside, including detached tasks, belongs to the scope: when the
/// scope future is dropped (for example by [`with_timeout`](crate::with_timeout)) the
/// whole subtree is cancelled.
///
/// # Panics
/// As [`spawn`].
pub async fn scope<T: 'static>(name: &str, future: impl Future<Output = T> + 'static) -> T {
    spawn(name, future).await
}

/// Handle to a spawned task; awaiting it yields the task's result.
///
/// Dropping the handle cancels the task (drops its future) unless it was
/// [detached](Self::detach). Awaiting is cancel-safe: dropping the await cancels the task,
/// it never loses a completed result to someone else.
///
/// Dropping a handle that was never awaited, before its task ever ran, fails the run: the
/// task would silently never run. The exception is the poll in which `main` returns (an
/// early `return` or `?` from the test): the run ends there, with `main`'s result. Call [`detach`](Self::detach) to run it in the
/// background, `.await` it, or [`cancel`](Self::cancel) it.
#[must_use = "dropping an unawaited Task cancels it before it runs and fails the test \
              (`let _ = spawn(..)` does this); call .detach() to run it in the background, \
              .await it, or .cancel() it"]
pub struct Task<T> {
    sim_gen: u32,
    key: Option<Key>,
    name: Rc<str>,
    join: Option<Registration>,
    /// The `spawn` call, where a failure for a dropped handle points.
    spawned_at: &'static Location<'static>,
    _output: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for Task<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task")
            .field("name", &self.name)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<T: 'static> Task<T> {
    /// The task's hierarchical name (`main.driver`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the task's future has completed.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        let Some(key) = self.key else { return true };
        state::with_sim(self.sim_gen, |k| {
            k.tasks
                .borrow()
                .get(key)
                .is_none_or(|e| e.state == TaskState::Done)
        })
        .unwrap_or(true)
    }

    /// Lets the task run on without this handle; it is cancelled when its owner completes
    /// or is cancelled. The owner is the task's parent, or, if the parent already
    /// completed, the nearest ancestor still running, so a task never outlives the
    /// [`scope`] it was spawned in.
    pub fn detach(mut self) {
        self.join = None;
        let Some(key) = self.key.take() else { return };
        state::with_sim(self.sim_gen, |k| {
            let mut tasks = k.tasks.borrow_mut();
            let Some(entry) = tasks.get_mut(key) else {
                return;
            };
            entry.has_handle = false;
            if entry.state == TaskState::Done {
                drop(tasks);
                free_task(k, key);
                return;
            }
            let ancestors = Rc::clone(&entry.ancestors);
            let owner = ancestors.iter().rev().copied().find(|&a| {
                tasks
                    .get(a)
                    .is_some_and(|e| matches!(e.state, TaskState::Pending | TaskState::Polling))
            });
            // No running ancestor means `main` completed: the run is ending, and `drop_all`
            // cancels the task.
            if let Some(owner) = owner {
                if let Some(e) = tasks.get_mut(key) {
                    e.owner = Some(owner);
                }
                if let Some(o) = tasks.get_mut(owner) {
                    o.detached.push(key);
                }
            }
        });
    }

    /// Cancels the task now: its future is dropped, running its `Drop` cleanup.
    pub fn cancel(mut self) {
        self.release(Release::Cancel);
    }

    pub(crate) const fn key(&self) -> Option<Key> {
        self.key
    }

    pub(crate) fn take_result(&mut self, k: &Kernel) -> Option<T> {
        let key = self.key?;
        let mut tasks = k.tasks.borrow_mut();
        let entry = tasks.get_mut(key)?;
        if entry.state != TaskState::Done {
            return None;
        }
        let boxed = entry.result.take()?;
        drop(tasks);
        let value = *boxed
            .downcast::<T>()
            .or_invariant("invariant: a task's result has the handle's type");
        self.key = None;
        self.join = None;
        free_task(k, key);
        Some(value)
    }
}

impl<T: 'static> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = &mut *self;
        let key = this
            .key
            .or_invariant("Task polled after it returned its result");
        let found = state::with_sim(this.sim_gen, |k| {
            if let Some(v) = this.take_result(k) {
                return Some(Poll::Ready(v));
            }
            let join = k.tasks.borrow().get(key).map(|e| e.join);
            let Some(join) = join else {
                panic!("task `{}` was cancelled while awaited", this.name)
            };
            match &this.join {
                Some(r) => {
                    r.poll_fired(cx);
                }
                None => this.join = Some(Registration::new(k, WaitOn::List(join), cx, None)),
            }
            Some(Poll::Pending)
        });
        match found.flatten() {
            Some(p) => p,
            None => panic!(
                "task `{}` awaited outside the simulation that spawned it",
                this.name
            ),
        }
    }
}

/// Why a handle lets go of its task.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Release {
    /// [`Task::cancel`]: deliberate.
    Cancel,
    /// The handle was dropped; dropping one never awaited, whose task never ran, fails.
    Drop,
}

impl<T> Task<T> {
    /// Gives up the handle: frees a finished task, cancels a running one.
    fn release(&mut self, why: Release) {
        let awaited = self.join.take().is_some();
        let Some(key) = self.key.take() else { return };
        state::with_sim(self.sim_gen, |k| {
            let (done, never_ran) = {
                let mut tasks = k.tasks.borrow_mut();
                let Some(entry) = tasks.get_mut(key) else {
                    return;
                };
                entry.has_handle = false;
                (entry.state == TaskState::Done, !entry.polled)
            };
            if done {
                free_task(k, key);
                return;
            }
            if why == Release::Drop && never_ran && !awaited && !cascading(k) {
                dropped_unpolled(k, key, self.spawned_at);
            }
            cancel_task(k, key);
        });
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.release(Release::Drop);
    }
}

/// Whether a handle drop is a side effect of a larger cancellation rather than the
/// testbench's own choice: the run is ending, the dropping task is itself being
/// cancelled (its locals go with it), a combinator is cancelling an operand (a `Task`
/// passed by value to `first!` or `with_timeout` that lost), or a panic is unwinding.
fn cascading(k: &Kernel) -> bool {
    let owner_cancelled = k.current.get().is_some_and(|c| {
        k.tasks
            .borrow()
            .get(c)
            .is_some_and(|e| e.state == TaskState::Cancelling)
    });
    k.phase.get() == Phase::Ended
        || owner_cancelled
        || k.cancelling_operand.get()
        || std::thread::panicking()
}

/// A handle dropped before its task ever ran, waiting for the end of the poll that dropped
/// it (`settle_unrun_drops`).
pub(crate) struct UnrunDrop {
    name: String,
    /// The task whose poll dropped the handle, named now: a task that finishes in that
    /// poll is freed before the poll is settled.
    dropper: Option<String>,
    spawned_at: &'static Location<'static>,
}

/// Notes a task cancelled by dropping its handle before it ever ran; the poll that dropped
/// it decides, when it ends, whether the run fails (`settle_unrun_drops`).
#[cold]
fn dropped_unpolled(k: &Kernel, key: Key, spawned_at: &'static Location<'static>) {
    k.record(TraceKind::DroppedUnpolled, Some(key), 0, 0);
    let dropper = k.current.get().map(|c| k.task_name(c));
    k.unrun_drops.borrow_mut().push(UnrunDrop {
        name: k.task_name(key),
        dropper,
        spawned_at,
    });
}

/// Ends the poll of `key` (`None`: the end of the run loop): fails the run for the handles it dropped before their tasks
/// ever ran, all named in one failure, unless that poll returned from `main`.
///
/// A failure, not a warning: the usual cause is `let _ = spawn(..)` (rustc's `must_use`
/// help suggests it), which cancels a driver or monitor before it does anything, and a
/// warning on stderr is hidden by test runners that capture output, so the test would pass
/// without the task (a false pass; correctness over ergonomics, constitution priorities).
///
/// The exception is the poll in which `main` returns, typically an early `return` or `?`
/// with the test's own error: that error is the run's result, not a `Setup` failure that
/// masks it. No pass is false there: the run ends when `main` returns (`run_ready` polls
/// nothing after it), so a detached task spawned in that poll would not have run either.
/// Every other task gets no exception: a handle dropped in a child's last poll, had it been
/// detached, would have run under the child's ancestors.
///
/// Waiting for the end of the poll is what lets every handle dropped together be named,
/// and lets a panic or another failure of the same poll win. Dropping stays the one
/// cancellation mechanism (DECISION §1): [`Task::cancel`] drops the future the same way,
/// and says it is deliberate. The trace record keeps every event in run directories.
pub(crate) fn settle_unrun_drops(k: &Kernel, key: Option<Key>) {
    let drops = std::mem::take(&mut *k.unrun_drops.borrow_mut());
    let Some(first) = drops.first() else { return };
    if key.is_some() && k.main.get() == key && k.main_done.get() {
        return;
    }
    let names: Vec<String> = drops
        .iter()
        .map(|d| {
            format!(
                "`{}` (spawned at {}:{})",
                d.name,
                d.spawned_at.file(),
                d.spawned_at.line()
            )
        })
        .collect();
    let (handles, were, tasks, it) = if drops.len() == 1 {
        ("the Task handle of", "was", "the task", "it was")
    } else {
        ("the Task handles of", "were", "the tasks", "they were")
    };
    let message = format!(
        "{handles} {} {were} dropped before {tasks} ran, so {it} cancelled: by `let _ = \
         spawn(..)`, or by leaving the handle's scope early (`?`, `return`, `break`); the \
         location is the `spawn` call of `{}`",
        names.join(", "),
        first.name,
    );
    k.fail_named(
        FailureKind::Setup {
            message: message.clone(),
            help: "call .detach() on the handle to run the task in the background, .await it \
                   for its result, or call .cancel() if cancelling it is intended (before \
                   an early return too, or spawn it after the checks that may return)",
        },
        first.dropper.clone(),
        Some(first.spawned_at.into()),
        message,
    );
}

/// Removes a task's entry and its join list; its future must already be gone.
fn free_task(k: &Kernel, key: Key) {
    let removed = k.tasks.borrow_mut().remove_task(key);
    let Some(entry) = removed else { return };
    if let Some(owner) = entry.owner
        && let Some(o) = k.tasks.borrow_mut().get_mut(owner)
    {
        o.detached.retain(|&c| c != key);
    }
    k.waits.borrow_mut().free_list(entry.join);
    // The result may run user `Drop` code; no borrow is held here.
    drop_guarded(k, key, entry.result);
}

/// Drops `value` (a future or a result) as task `key`, turning a panic into a failure.
///
/// When this runs during another panic's unwinding (a task's poll panicked, and unwinding
/// drops the handles in its locals), that first panic is the cause. A second panic from
/// this cleanup is then printed but not recorded, and the first panic's location is kept,
/// so the failure names the first panic.
fn drop_guarded<V>(k: &Kernel, key: Key, value: V) {
    let unwinding = std::thread::panicking();
    let outer = panics::take();
    let previous = k.current.replace(Some(key));
    let outcome = panic::catch_unwind(AssertUnwindSafe(move || drop(value)));
    k.current.set(previous);
    let location = panics::take();
    if unwinding {
        panics::restore(outer);
    }
    let Err(payload) = outcome else { return };
    let message = format!("panicked while being dropped: {}", panic_message(&*payload));
    if unwinding {
        eprintln!(
            "simjig: task `{}` {message}, while another panic was unwinding; the first \
             panic is the one reported",
            k.task_name(key)
        );
        return;
    }
    k.fail(
        FailureKind::TaskPanic {
            message: message.clone(),
        },
        Some(key),
        location,
        message,
    );
}

/// Cancels a task: drops its future (running its cleanup), then its detached children.
pub(crate) fn cancel_task(k: &Kernel, key: Key) {
    let (future, children) = {
        let mut tasks = k.tasks.borrow_mut();
        let Some(entry) = tasks.get_mut(key) else {
            return;
        };
        match entry.state {
            TaskState::Polling => {
                entry.cancel_requested = true;
                return;
            }
            TaskState::Done | TaskState::Cancelling => return,
            TaskState::Pending => {}
        }
        entry.state = TaskState::Cancelling;
        (entry.future.take(), std::mem::take(&mut entry.detached))
    };
    k.record(TraceKind::Cancel, Some(key), 0, 0);
    drop_guarded(k, key, future);
    for child in children {
        cancel_task(k, child);
    }
    free_task(k, key);
}

fn complete_task(k: &Kernel, key: Key, future: BoxFuture) {
    drop_guarded(k, key, future);
    k.record(TraceKind::Done, Some(key), 0, 0);
    let (children, join, has_handle) = {
        let mut tasks = k.tasks.borrow_mut();
        let entry = tasks
            .get_mut(key)
            .or_invariant("invariant: a completing task is in the table");
        entry.state = TaskState::Done;
        (
            std::mem::take(&mut entry.detached),
            entry.join,
            entry.has_handle,
        )
    };
    if k.main.get() == Some(key) {
        k.main_done.set(true);
    }
    super::waits::wake_all(k, join);
    for child in children {
        cancel_task(k, child);
    }
    if !has_handle {
        free_task(k, key);
    }
}

pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

/// Polls one task.
///
/// A woken helper polls its root task routed to it (`combinator`); a helper that already
/// finished, or whose root is not waiting, is skipped.
fn poll_task(k: &Kernel, woken: Key) {
    let (key, route) = {
        let tasks = k.tasks.borrow();
        match tasks.helper(woken) {
            Some(h) if h.state == HelperState::Done => return,
            Some(h) => (h.root, Some(woken)),
            None => (woken, None),
        }
    };
    let future = {
        let mut tasks = k.tasks.borrow_mut();
        let Some(entry) = tasks.get_mut(key) else {
            return;
        };
        if entry.state != TaskState::Pending {
            return;
        }
        entry.state = TaskState::Polling;
        entry.polled = true;
        entry.future.take()
    };
    let Some(mut future) = future else { return };
    k.record(TraceKind::Poll, Some(key), u64::from(route.is_some()), 0);
    k.bump(|c| c.polls += 1);
    let waker = waker::task_waker(k.sim_gen, key.index, gen16(key.gen_));
    let mut cx = Context::from_waker(&waker);
    let previous = k.current.replace(Some(key));
    let context = k.context.replace(Some(key));
    let previous_route = k.route.replace(route);
    panics::forget();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut cx)));
    k.current.set(previous);
    k.context.set(context);
    k.route.set(previous_route);
    finish_poll(k, key, future, outcome);
    settle_unrun_drops(k, Some(key));
}

/// Stores, completes or fails the task `key` after a poll returned `outcome`.
fn finish_poll(k: &Kernel, key: Key, future: BoxFuture, outcome: std::thread::Result<Poll<()>>) {
    match outcome {
        Ok(Poll::Ready(())) => complete_task(k, key, future),
        Ok(Poll::Pending) => {
            let cancel = {
                let mut tasks = k.tasks.borrow_mut();
                let entry = tasks
                    .get_mut(key)
                    .or_invariant("invariant: a polled task stays in the table");
                entry.future = Some(future);
                entry.state = TaskState::Pending;
                entry.cancel_requested
            };
            if cancel {
                cancel_task(k, key);
            }
        }
        Err(payload) => {
            let location = panics::take();
            // Keep the future: it is dropped with the rest when the run ends.
            if let Some(entry) = k.tasks.borrow_mut().get_mut(key) {
                entry.future = Some(future);
                entry.state = TaskState::Pending;
            }
            let message = panic_message(&*payload);
            k.fail(
                FailureKind::TaskPanic {
                    message: message.clone(),
                },
                Some(key),
                location,
                format!("panicked: {message}"),
            );
        }
    }
}

/// Polls ready tasks in FIFO order until none is ready, a failure is recorded, or the
/// main task completes.
pub(crate) fn run_ready(k: &Kernel) {
    while !k.failed() && !k.main_done.get() {
        let Some((index, g)) = k.ready.borrow_mut().pop() else {
            return;
        };
        let key = k.tasks.borrow().by_wake(index, g);
        if let Some(key) = key {
            poll_task(k, key);
        }
    }
}

/// Starts the `N` helpers of a combinator awaited in the current context and queues them
/// at the front, in operand order, as cocotb's `_wait` does (`_start_next` on each, last
/// first). Returns the combinator's owner (the current context) and the helpers.
///
/// # Panics
/// Outside a task's poll, or if 65 536 tasks and helpers are alive at once.
pub(crate) fn start_helpers<const N: usize>(k: &Kernel) -> (Key, [Key; N]) {
    let root = k
        .current
        .get()
        .or_invariant("invariant: a combinator is polled by a task");
    let owner = k.context.get().unwrap_or(root);
    let mut tasks = k.tasks.borrow_mut();
    let keys: [Key; N] = std::array::from_fn(|_| {
        tasks.slab.insert(Entry::Helper(Helper {
            root,
            owner,
            state: HelperState::Running,
            live_forks: 0,
            propagated: false,
        }))
    });
    tasks.helpers += N;
    if let Some(o) = tasks.helper_mut(owner) {
        o.live_forks += 1;
    }
    drop(tasks);
    let mut ready = k.ready.borrow_mut();
    for key in keys.iter().rev() {
        assert!(
            key.index <= MAX_TASK_INDEX,
            "more than {} tasks and combinator operands alive at once",
            MAX_TASK_INDEX + 1
        );
        ready.push_front(key.index, gen16(key.gen_));
    }
    (owner, keys)
}

/// Whether this poll runs the context that polls the combinator (its task, or its
/// helper), rather than passing through it to a helper of a combinator inside.
pub(crate) fn runs_owner(k: &Kernel) -> bool {
    let route = k.route.get();
    route.is_none() || route == k.context.get()
}

/// The operand whose helper this run is routed to, directly or through combinators nested
/// in that operand.
pub(crate) fn route_index(k: &Kernel, helpers: &[Key]) -> Option<usize> {
    let mut at = k.route.get()?;
    let tasks = k.tasks.borrow();
    loop {
        if let Some(i) = helpers.iter().position(|&h| h == at) {
            return Some(i);
        }
        at = tasks.helper(at)?.owner;
    }
}

/// Whether this run is helper `key`'s own (not one passing through it).
pub(crate) fn routed_to(k: &Kernel, key: Key) -> bool {
    k.route.get() == Some(key)
}

pub(crate) fn helper_state(k: &Kernel, key: Key) -> HelperState {
    k.tasks
        .borrow()
        .helper(key)
        .map_or(HelperState::Done, |h| h.state)
}

/// Enters helper `key`'s context; returns the waker its operand must register and the
/// context to restore afterwards.
pub(crate) fn enter_helper(k: &Kernel, key: Key) -> (Waker, Option<Key>) {
    let waker = waker::task_waker(k.sim_gen, key.index, gen16(key.gen_));
    (waker, k.context.replace(Some(key)))
}

/// cocotb's `Task.cancel()` on a helper: marks it cancelling and queues it at the back,
/// or leaves it where it already is queued (a scheduled task is "hijacked"). Returns
/// whether its operand can be dropped now, deregistering its trigger as cocotb does at
/// once: not when the operand awaits started combinators, which cancel their own
/// helpers only when this one runs.
pub(crate) fn cancel_helper(k: &Kernel, key: Key) -> bool {
    let drop_now = {
        let mut tasks = k.tasks.borrow_mut();
        let Some(h) = tasks.helper_mut(key) else {
            return false;
        };
        h.state = HelperState::Cancelling;
        h.live_forks == 0
    };
    k.ready.borrow_mut().push(key.index, gen16(key.gen_));
    drop_now
}

/// For a run of cancelling helper `key`: whether the cancellation must still be passed to
/// the started combinators in its operand. True once.
pub(crate) fn take_propagation(k: &Kernel, key: Key) -> bool {
    let mut tasks = k.tasks.borrow_mut();
    let Some(h) = tasks.helper_mut(key) else {
        return false;
    };
    let propagate = h.live_forks > 0 && !h.propagated;
    h.propagated = true;
    propagate
}

/// Whether helper `key`'s operand still awaits a combinator with helpers running.
pub(crate) fn has_live_forks(k: &Kernel, key: Key) -> bool {
    k.tasks
        .borrow()
        .helper(key)
        .is_some_and(|h| h.live_forks > 0)
}

/// Queues `key` at the back again (cocotb awaiting `NullTrigger`).
pub(crate) fn requeue(k: &Kernel, key: Key) {
    k.ready.borrow_mut().push(key.index, gen16(key.gen_));
}

pub(crate) fn finish_helper(k: &Kernel, key: Key) {
    if let Some(h) = k.tasks.borrow_mut().helper_mut(key) {
        h.state = HelperState::Done;
    }
}

/// Whether `owner` is a helper being cancelled: its combinators cancel their helpers.
pub(crate) fn owner_cancelling(k: &Kernel, owner: Key) -> bool {
    k.tasks
        .borrow()
        .helper(owner)
        .is_some_and(|h| h.state == HelperState::Cancelling)
}

/// A combinator's last helper finished: wakes its owner (cocotb's `complete.set()`),
/// unless the owner is a helper being cancelled, which no longer waits on it.
pub(crate) fn fork_complete(k: &Kernel, owner: Key) {
    {
        let mut tasks = k.tasks.borrow_mut();
        if let Some(h) = tasks.helper_mut(owner) {
            h.live_forks -= 1;
            if h.state != HelperState::Running {
                return;
            }
        } else if tasks.get(owner).is_none() {
            return;
        }
    }
    k.ready.borrow_mut().push(owner.index, gen16(owner.gen_));
}

/// Removes a combinator's helpers; `live` if some had not finished.
pub(crate) fn free_helpers(k: &Kernel, helpers: &[Key], owner: Key, live: bool) {
    let mut tasks = k.tasks.borrow_mut();
    for &key in helpers {
        if tasks.helper(key).is_some() {
            tasks.slab.remove(key);
            tasks.helpers -= 1;
        }
    }
    if live && let Some(o) = tasks.helper_mut(owner) {
        o.live_forks -= 1;
    }
}

/// Drops every task, children through their parents first, until the table is empty.
pub(crate) fn drop_all(k: &Kernel) {
    loop {
        let keys = k.tasks.borrow().keys();
        let Some(&first) = keys.first() else { return };
        for key in keys {
            let done = k.tasks.borrow().get(key).map(|e| e.state);
            match done {
                Some(TaskState::Done) => free_task(k, key),
                Some(_) => cancel_task(k, key),
                None => {}
            }
        }
        // A `Drop` that keeps spawning would loop forever; one pass must make progress.
        if k.tasks.borrow().get(first).is_some() {
            return;
        }
    }
}
