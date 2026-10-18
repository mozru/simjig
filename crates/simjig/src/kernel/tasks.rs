//! Tasks: spawned futures with hierarchical names, owned by a handle or by their parent.
//!
//! # Ownership and cancellation (DECISION §1)
//! - [`spawn`] returns a [`Task<T>`] handle that owns the task: dropping the handle cancels
//!   the task. Cancellation is *only* dropping its future; cleanup is synchronous `Drop`,
//!   which may still write signals (through the write log) or release locks.
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

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use super::OrInvariant;
use super::failure::FailureKind;
use super::panics;
use super::recorder::TraceKind;
use super::slab::{Key, Slab};
use super::state::{self, Kernel};
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
    cancel_requested: bool,
    detached: Vec<Key>,
    /// Wait list the handle registers on to learn of completion.
    join: Key,
}

#[derive(Default)]
pub(crate) struct TaskTable {
    slab: Slab<TaskEntry>,
}

impl TaskTable {
    pub(crate) fn name(&self, key: Key) -> Option<Rc<str>> {
        self.slab.get(key).map(|e| Rc::clone(&e.name))
    }

    pub(crate) fn len(&self) -> usize {
        self.slab.len()
    }

    /// Keys and names of tasks whose futures have not completed, in slot order.
    pub(crate) fn unfinished(&self) -> Vec<(Key, Rc<str>)> {
        self.slab
            .iter()
            .filter(|(_, e)| e.state != TaskState::Done)
            .map(|(k, e)| (k, Rc::clone(&e.name)))
            .collect()
    }

    pub(crate) fn keys(&self) -> Vec<Key> {
        self.slab.iter().map(|(k, _)| k).collect()
    }

    fn by_wake(&self, index: u32, gen16_: u16) -> Option<Key> {
        self.slab.key_at(index).filter(|k| gen16(k.gen_) == gen16_)
    }
}

impl fmt::Debug for TaskTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.slab.iter().map(|(_, e)| &e.name))
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
    pub(crate) fn push(&mut self, index: u32, gen16_: u16) {
        let i = index as usize;
        if self.queued.len() <= i {
            self.queued.resize(i + 1, None);
        }
        if self.queued[i] == Some(gen16_) {
            return;
        }
        self.queued[i] = Some(gen16_);
        self.queue.push_back((index, gen16_));
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
/// already queued. The returned handle owns it: drop the handle to cancel the task, await
/// it for the result, or [`detach`](Task::detach) it.
///
/// # Panics
/// Outside [`Sim::run`](crate::Sim::run), or if 65 536 tasks are alive at once.
#[track_caller]
pub fn spawn<T: 'static>(name: &str, future: impl Future<Output = T> + 'static) -> Task<T> {
    state::with_current("spawn", |k| {
        let parent = k.current.get();
        spawn_in(k, parent, name, future)
    })
}

pub(crate) fn spawn_in<T: 'static>(
    k: &Kernel,
    parent: Option<Key>,
    name: &str,
    future: impl Future<Output = T> + 'static,
) -> Task<T> {
    let (full, ancestors): (Rc<str>, Rc<[Key]>) = {
        let tasks = k.tasks.borrow();
        match parent.and_then(|p| Some((p, tasks.slab.get(p)?))) {
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
            if let Some(e) = k.tasks.borrow_mut().slab.get_mut(key) {
                e.result = Some(Box::new(value));
            }
        });
    });
    let key = k.tasks.borrow_mut().slab.insert(TaskEntry {
        name: Rc::clone(&full),
        ancestors,
        owner: None,
        state: TaskState::Pending,
        future: Some(wrapped),
        result: None,
        has_handle: true,
        cancel_requested: false,
        detached: Vec::new(),
        join,
    });
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
#[must_use = "dropping a Task cancels it; await it or call .detach()"]
pub struct Task<T> {
    sim_gen: u32,
    key: Option<Key>,
    name: Rc<str>,
    join: Option<Registration>,
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
                .slab
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
            let Some(entry) = tasks.slab.get_mut(key) else {
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
                    .slab
                    .get(a)
                    .is_some_and(|e| matches!(e.state, TaskState::Pending | TaskState::Polling))
            });
            // No running ancestor means `main` completed: the run is ending, and `drop_all`
            // cancels the task.
            if let Some(owner) = owner {
                if let Some(e) = tasks.slab.get_mut(key) {
                    e.owner = Some(owner);
                }
                if let Some(o) = tasks.slab.get_mut(owner) {
                    o.detached.push(key);
                }
            }
        });
    }

    /// Cancels the task now: its future is dropped, running its `Drop` cleanup.
    pub fn cancel(self) {
        drop(self);
    }

    pub(crate) const fn key(&self) -> Option<Key> {
        self.key
    }

    pub(crate) fn take_result(&mut self, k: &Kernel) -> Option<T> {
        let key = self.key?;
        let mut tasks = k.tasks.borrow_mut();
        let entry = tasks.slab.get_mut(key)?;
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
            let join = k.tasks.borrow().slab.get(key).map(|e| e.join);
            let Some(join) = join else {
                panic!("task `{}` was cancelled while awaited", this.name)
            };
            match &this.join {
                Some(r) => {
                    r.poll_fired(cx);
                }
                None => this.join = Some(Registration::new(k, WaitOn::List(join), cx)),
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

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.join = None;
        let Some(key) = self.key.take() else { return };
        state::with_sim(self.sim_gen, |k| {
            let done = {
                let mut tasks = k.tasks.borrow_mut();
                let Some(entry) = tasks.slab.get_mut(key) else {
                    return;
                };
                entry.has_handle = false;
                entry.state == TaskState::Done
            };
            if done {
                free_task(k, key);
            } else {
                cancel_task(k, key);
            }
        });
    }
}

/// Removes a task's entry and its join list; its future must already be gone.
fn free_task(k: &Kernel, key: Key) {
    let removed = k.tasks.borrow_mut().slab.remove(key);
    let Some(entry) = removed else { return };
    if let Some(owner) = entry.owner
        && let Some(o) = k.tasks.borrow_mut().slab.get_mut(owner)
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
        let Some(entry) = tasks.slab.get_mut(key) else {
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
            .slab
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
fn poll_task(k: &Kernel, key: Key) {
    let future = {
        let mut tasks = k.tasks.borrow_mut();
        let Some(entry) = tasks.slab.get_mut(key) else {
            return;
        };
        if entry.state != TaskState::Pending {
            return;
        }
        entry.state = TaskState::Polling;
        entry.future.take()
    };
    let Some(mut future) = future else { return };
    k.record(TraceKind::Poll, Some(key), 0, 0);
    k.bump(|c| c.polls += 1);
    let waker = waker::task_waker(k.sim_gen, key.index, gen16(key.gen_));
    let mut cx = Context::from_waker(&waker);
    let previous = k.current.replace(Some(key));
    panics::forget();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut cx)));
    k.current.set(previous);
    match outcome {
        Ok(Poll::Ready(())) => complete_task(k, key, future),
        Ok(Poll::Pending) => {
            let cancel = {
                let mut tasks = k.tasks.borrow_mut();
                let entry = tasks
                    .slab
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
            if let Some(entry) = k.tasks.borrow_mut().slab.get_mut(key) {
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

/// Drops every task, children through their parents first, until the table is empty.
pub(crate) fn drop_all(k: &Kernel) {
    loop {
        let keys = k.tasks.borrow().keys();
        let Some(&first) = keys.first() else { return };
        for key in keys {
            let done = k.tasks.borrow().slab.get(key).map(|e| e.state);
            match done {
                Some(TaskState::Done) => free_task(k, key),
                Some(_) => cancel_task(k, key),
                None => {}
            }
        }
        // A `Drop` that keeps spawning would loop forever; one pass must make progress.
        if k.tasks.borrow().slab.get(first).is_some() {
            return;
        }
    }
}
