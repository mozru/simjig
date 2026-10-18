//! Synchronization between tasks: [`Event`], [`Lock`] and the bounded [`Queue`].
//!
//! Each primitive keeps its waiters in kernel wait lists, so a blocked task shows up in
//! `Deadlock` reports with what it waits for, and every waiting future deregisters in
//! `Drop`. Waiters are woken first-come first-served, which keeps runs deterministic.
//! Handles are cheap `Rc` clones; move a clone into each task that needs it.
//!
//! # Readiness, as in cocotb 2.1 (goldens `event_ready`, `lock_ready`)
//! Awaiting a set [`Event`] or a free [`Lock`] does not continue at once: cocotb's
//! `_Event._prime` and `Lock._prime_lock` fire the trigger while it is being primed, which
//! schedules the task at the back of the ready queue. Here the wait takes the same hop
//! ([`yield_now`]); a free lock is taken before the hop, so tasks that run in between see
//! it locked. [`Queue::get`] and [`Queue::put`] take no hop when they can complete (they
//! await nothing in cocotb), and a woken getter or putter that finds its item or slot
//! taken by a task that ran first waits again, last in line.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use super::OrInvariant;
use super::slab::Key;
use super::state::{self, Kernel};
use super::trigger::{YieldNow, yield_now};
use super::waits::{self, ListKind, Registration, WaitOn};

/// A wait list owned by a primitive, freed when the last handle drops.
struct OwnedList {
    sim_gen: u32,
    key: Key,
    name: Rc<str>,
}

impl OwnedList {
    #[track_caller]
    fn new(kind: ListKind, name: &str) -> Self {
        let name: Rc<str> = name.into();
        state::with_current("creating an Event, Lock or Queue", |k| Self {
            sim_gen: k.sim_gen,
            key: k.waits.borrow_mut().alloc_list(kind, Rc::clone(&name)),
            name,
        })
    }

    /// Runs `f` on the owning kernel.
    ///
    /// # Panics
    /// Outside the primitive's own running simulation.
    #[track_caller]
    fn with<R>(&self, f: impl FnOnce(&Kernel) -> R) -> R {
        match state::with_sim(self.sim_gen, f) {
            Some(r) => r,
            None => panic!(
                "`{}` belongs to simulation #{} and is used outside it",
                self.name, self.sim_gen
            ),
        }
    }

    // These two run from `Drop` paths too, so outside the simulation they do nothing.
    fn has_waiters(&self) -> bool {
        state::with_sim(self.sim_gen, |k| {
            k.waits.borrow().list_has_waiters(self.key)
        })
        .unwrap_or(false)
    }

    fn wake_one(&self) -> bool {
        state::with_sim(self.sim_gen, |k| waits::wake_one(k, self.key)).unwrap_or(false)
    }

    fn register(&self, cx: &Context<'_>) -> Registration {
        self.with(|k| Registration::new(k, WaitOn::List(self.key), cx, None))
    }
}

impl Drop for OwnedList {
    fn drop(&mut self) {
        state::with_sim(self.sim_gen, |k| k.waits.borrow_mut().free_list(self.key));
    }
}

struct EventInner {
    list: OwnedList,
    set: Cell<bool>,
}

/// A flag tasks can wait on: [`wait`](Self::wait) returns once the event is set.
#[derive(Clone)]
pub struct Event {
    inner: Rc<EventInner>,
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Event")
            .field("name", &self.inner.list.name)
            .field("set", &self.inner.set.get())
            .finish()
    }
}

impl Event {
    /// A cleared event named `name` (used in diagnostics).
    ///
    /// # Panics
    /// Outside [`Sim::run`](crate::Sim::run).
    #[track_caller]
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            inner: Rc::new(EventInner {
                list: OwnedList::new(ListKind::Event, name),
                set: Cell::new(false),
            }),
        }
    }

    /// Sets the event and wakes every waiter. Waiters woken resume even if the event is
    /// cleared before they run.
    ///
    /// # Panics
    /// Outside the event's own running simulation.
    #[track_caller]
    pub fn set(&self) {
        self.inner.set.set(true);
        let list = &self.inner.list;
        list.with(|k| waits::wake_all(k, list.key));
    }

    /// Clears the event; later [`wait`](Self::wait)s block until it is set again.
    pub fn clear(&self) {
        self.inner.set.set(false);
    }

    /// Whether the event is set.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.inner.set.get()
    }

    /// Waits until the event is set. If it already is, resumes after the tasks already
    /// ready (one hop, as cocotb), even if the event is cleared meanwhile.
    ///
    /// # Cancellation
    /// Cancel-safe.
    pub fn wait(&self) -> EventWait<'_> {
        EventWait {
            event: self,
            state: EventState::Fresh,
        }
    }
}

/// Future returned by [`Event::wait`].
#[derive(Debug)]
#[must_use = "futures do nothing unless awaited"]
pub struct EventWait<'a> {
    event: &'a Event,
    state: EventState,
}

#[derive(Debug)]
enum EventState {
    Fresh,
    /// The event was set when awaited.
    Hop(YieldNow),
    Waiting(Registration),
}

impl Future for EventWait<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let state = match std::mem::replace(&mut self.state, EventState::Fresh) {
            EventState::Fresh if self.event.is_set() => EventState::Hop(yield_now()),
            EventState::Fresh => EventState::Waiting(self.event.inner.list.register(cx)),
            started => started,
        };
        match state {
            EventState::Hop(mut hop) => {
                if Pin::new(&mut hop).poll(cx).is_ready() {
                    return Poll::Ready(());
                }
                self.state = EventState::Hop(hop);
            }
            EventState::Waiting(r) if r.poll_fired(cx) => return Poll::Ready(()),
            waiting => self.state = waiting,
        }
        Poll::Pending
    }
}

struct LockInner {
    list: OwnedList,
    locked: Cell<bool>,
}

impl LockInner {
    /// Hands the lock to the oldest waiter, or unlocks it if nobody waits.
    fn release(&self) {
        if !self.list.wake_one() {
            self.locked.set(false);
        }
    }
}

/// A mutual-exclusion lock, granted to waiters in arrival order.
#[derive(Clone)]
pub struct Lock {
    inner: Rc<LockInner>,
}

impl fmt::Debug for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lock")
            .field("name", &self.inner.list.name)
            .field("locked", &self.inner.locked.get())
            .finish()
    }
}

impl Lock {
    /// An unlocked lock named `name` (used in diagnostics).
    ///
    /// # Panics
    /// Outside [`Sim::run`](crate::Sim::run).
    #[track_caller]
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            inner: Rc::new(LockInner {
                list: OwnedList::new(ListKind::Lock, name),
                locked: Cell::new(false),
            }),
        }
    }

    /// Waits for the lock. A free lock is taken at once, and the task resumes after the
    /// tasks already ready (one hop, as cocotb). The guard releases it when dropped,
    /// including when its task is cancelled.
    ///
    /// # Cancellation
    /// Cancel-safe: a lock granted to a future that is dropped before it resumes is passed
    /// on to the next waiter (cocotb keeps it held by the cancelled task; conformance README,
    /// "Known differences").
    pub fn lock(&self) -> LockFuture<'_> {
        LockFuture {
            lock: self,
            state: LockState::Fresh,
        }
    }

    /// Takes the lock if it is free and nobody is waiting for it.
    #[must_use]
    pub fn try_lock(&self) -> Option<LockGuard<'_>> {
        if self.inner.locked.get() || self.inner.list.has_waiters() {
            return None;
        }
        self.inner.locked.set(true);
        Some(LockGuard { lock: self })
    }

    /// Whether some task holds the lock.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.inner.locked.get()
    }
}

/// Future returned by [`Lock::lock`].
#[derive(Debug)]
#[must_use = "futures do nothing unless awaited"]
pub struct LockFuture<'a> {
    lock: &'a Lock,
    state: LockState<'a>,
}

#[derive(Debug)]
enum LockState<'a> {
    Fresh,
    /// The lock was free when awaited: taken, and resumed after one hop.
    Hop(LockGuard<'a>, YieldNow),
    Waiting(Registration),
}

impl<'a> Future for LockFuture<'a> {
    type Output = LockGuard<'a>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<LockGuard<'a>> {
        let lock = self.lock;
        let state = match std::mem::replace(&mut self.state, LockState::Fresh) {
            LockState::Fresh => match lock.try_lock() {
                Some(guard) => LockState::Hop(guard, yield_now()),
                None => LockState::Waiting(lock.inner.list.register(cx)),
            },
            started => started,
        };
        match state {
            LockState::Hop(guard, mut hop) => {
                if Pin::new(&mut hop).poll(cx).is_ready() {
                    return Poll::Ready(guard);
                }
                self.state = LockState::Hop(guard, hop);
            }
            // Fired means granted: `release` kept the lock locked for us.
            LockState::Waiting(r) if r.poll_fired(cx) => return Poll::Ready(LockGuard { lock }),
            waiting => self.state = waiting,
        }
        Poll::Pending
    }
}

impl Drop for LockFuture<'_> {
    // A guard held through the hop releases in its own `Drop`.
    fn drop(&mut self) {
        if let LockState::Waiting(reg) = std::mem::replace(&mut self.state, LockState::Fresh)
            && reg.is_fired()
        {
            drop(reg);
            self.lock.inner.release();
        }
    }
}

/// Holds a [`Lock`]; dropping it releases the lock to the next waiter.
#[derive(Debug)]
#[must_use = "dropping the guard releases the lock at once"]
pub struct LockGuard<'a> {
    lock: &'a Lock,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        self.lock.inner.release();
    }
}

struct QueueInner<T> {
    items: RefCell<VecDeque<T>>,
    capacity: usize,
    getters: OwnedList,
    putters: OwnedList,
}

/// A FIFO queue between tasks, bounded or not. `put` waits while it is full, `get` while
/// it is empty; waiters are woken in arrival order. As in cocotb, a `get` (`put`) that can
/// complete does so at once, even ahead of a woken getter (putter) that has not run yet;
/// that one waits again, last in line.
pub struct Queue<T> {
    inner: Rc<QueueInner<T>>,
}

impl<T> Clone for Queue<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T> fmt::Debug for Queue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queue")
            .field("name", &self.inner.getters.name)
            .field("len", &self.inner.items.borrow().len())
            .field("capacity", &self.inner.capacity)
            .finish()
    }
}

impl<T> Queue<T> {
    /// An empty queue named `name` holding at most `capacity` items; a `capacity` of 0
    /// is unbounded (`put` never waits), as cocotb's `Queue(maxsize=0)` is.
    ///
    /// # Panics
    /// Outside [`Sim::run`](crate::Sim::run).
    #[track_caller]
    #[must_use]
    pub fn new(name: &str, capacity: usize) -> Self {
        Self {
            inner: Rc::new(QueueInner {
                items: RefCell::new(VecDeque::with_capacity(capacity)),
                capacity,
                getters: OwnedList::new(ListKind::QueueGet, name),
                putters: OwnedList::new(ListKind::QueuePut, name),
            }),
        }
    }

    /// Number of items in the queue.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.items.borrow().len()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The maximum number of items; 0 for an unbounded queue.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Whether a `put` must wait (cocotb's `full()`, which is never true for `maxsize=0`).
    fn is_full(&self) -> bool {
        self.inner.capacity != 0 && self.len() >= self.inner.capacity
    }

    fn push(&self, item: T) {
        self.inner.items.borrow_mut().push_back(item);
        self.inner.getters.wake_one();
    }

    fn pop(&self) -> Option<T> {
        let item = self.inner.items.borrow_mut().pop_front()?;
        self.inner.putters.wake_one();
        Some(item)
    }

    /// Adds `item` if there is room (cocotb's `put_nowait`); otherwise returns it.
    ///
    /// # Errors
    /// Gives `item` back when the queue is full.
    pub fn try_put(&self, item: T) -> Result<(), T> {
        if self.is_full() {
            return Err(item);
        }
        self.push(item);
        Ok(())
    }

    /// Removes the oldest item, if any.
    pub fn try_get(&self) -> Option<T> {
        self.pop()
    }

    /// Waits for room, then adds `item`.
    ///
    /// # Cancellation
    /// Cancel-safe: if dropped before it completes, `item` is dropped and not enqueued.
    pub fn put(&self, item: T) -> Put<'_, T> {
        Put {
            queue: self,
            item: Some(item),
            reg: None,
        }
    }

    /// Waits for an item and removes it.
    ///
    /// # Cancellation
    /// Cancel-safe: no item is lost if the future is dropped.
    pub fn get(&self) -> Get<'_, T> {
        Get {
            queue: self,
            reg: None,
        }
    }
}

/// Future returned by [`Queue::put`].
#[must_use = "futures do nothing unless awaited"]
pub struct Put<'a, T> {
    queue: &'a Queue<T>,
    item: Option<T>,
    reg: Option<Registration>,
}

// `Put` never pins `item`, so it is `Unpin` whatever `T` is.
impl<T> Unpin for Put<'_, T> {}

impl<T> fmt::Debug for Put<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Put").field("queue", self.queue).finish()
    }
}

impl<T> Future for Put<'_, T> {
    type Output = ();

    // cocotb's `while self.full(): wait` loop: a woken putter that finds the queue full
    // again registers anew, behind the others.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self;
        if this.reg.as_ref().is_some_and(|r| !r.poll_fired(cx)) {
            return Poll::Pending;
        }
        let queue = this.queue;
        if !queue.is_full() {
            this.reg = None;
            let item = this
                .item
                .take()
                .or_invariant("invariant: Put completes once");
            queue.push(item);
            return Poll::Ready(());
        }
        this.reg = Some(queue.inner.putters.register(cx));
        Poll::Pending
    }
}

impl<T> Drop for Put<'_, T> {
    fn drop(&mut self) {
        // A wake we will not use goes to the next putter.
        if let Some(reg) = self.reg.take()
            && reg.is_fired()
        {
            drop(reg);
            if !self.queue.is_full() {
                self.queue.inner.putters.wake_one();
            }
        }
    }
}

/// Future returned by [`Queue::get`].
#[must_use = "futures do nothing unless awaited"]
pub struct Get<'a, T> {
    queue: &'a Queue<T>,
    reg: Option<Registration>,
}

impl<T> fmt::Debug for Get<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Get").field("queue", self.queue).finish()
    }
}

impl<T> Future for Get<'_, T> {
    type Output = T;

    // cocotb's `while self.empty(): wait` loop, as for `Put`.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = &mut *self;
        if this.reg.as_ref().is_some_and(|r| !r.poll_fired(cx)) {
            return Poll::Pending;
        }
        let queue = this.queue;
        if !queue.is_empty() {
            this.reg = None;
            let item = queue
                .pop()
                .or_invariant("invariant: the queue is not empty");
            return Poll::Ready(item);
        }
        this.reg = Some(queue.inner.getters.register(cx));
        Poll::Pending
    }
}

impl<T> Drop for Get<'_, T> {
    fn drop(&mut self) {
        if let Some(reg) = self.reg.take()
            && reg.is_fired()
        {
            drop(reg);
            if !self.queue.is_empty() {
                self.queue.inner.getters.wake_one();
            }
        }
    }
}
