//! The wait table: every pending trigger registration, and the containers that order them.
//!
//! A trigger future registers a [`Registration`] on first poll and holds it until it
//! completes; the registration's `Drop` removes it, so a dropped (cancelled) trigger never
//! leaves a dangling wake. Containers are plain vectors and a `BTreeMap` time wheel, never
//! a `HashMap`, so iteration order is deterministic.
//!
//! Firing marks a registration as fired, removes it from its container and wakes its
//! waker. Wakes that happen together (after one eval, or at the start of a timestep) are
//! issued in registration order (`seq`), whatever container they came from.
//!
//! Value-change watching: a slot is *watched* while it has at least one edge waiter. On the
//! first registration the kernel snapshots the slot's value; the model only changes in
//! `eval` and when the write log is applied, and the kernel compares right after both, so
//! the snapshot is the value at the previous comparison. Only watched slots are compared.

use std::collections::BTreeMap;
use std::panic::Location;
use std::rc::Rc;
use std::task::{Context, Waker};

use super::OrInvariant;
use super::backend::words;
use super::models::Models;
use super::slab::{Key, Slab};
use super::state::{self, Kernel};
use super::time::{Precision, SimTime};

/// Which value changes an edge waiter wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeKind {
    Rising,
    Falling,
    Changed,
}

/// What a registration waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitOn {
    Edge { slot: u32, edge: EdgeKind },
    Time(SimTime),
    NextTimeStep,
    ReadWrite,
    ReadOnly,
    List(Key),
}

/// The owner of a wait list, for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListKind {
    Task,
    Event,
    Lock,
    QueueGet,
    QueuePut,
}

struct Waiter {
    task: Option<Key>,
    waker: Waker,
    fired: bool,
    seq: u64,
    on: WaitOn,
    /// Where the trigger was made, for triggers that know it.
    at: Option<&'static Location<'static>>,
}

struct WaitList {
    kind: ListKind,
    name: Rc<str>,
    waiters: Vec<Key>,
}

#[derive(Default)]
struct SlotWatch {
    waiters: Vec<Key>,
    snapshot: Vec<u32>,
}

/// What [`WaitTable::add`] needs to register a waiter.
struct NewWaiter {
    on: WaitOn,
    task: Option<Key>,
    waker: Waker,
    at: Option<&'static Location<'static>>,
}

/// A fired registration's wake, ordered by registration.
pub(crate) type Wake = (u64, Waker);

pub(crate) struct WaitTable {
    seq: u64,
    waiters: Slab<Waiter>,
    watch: Vec<SlotWatch>,
    /// Watched slots, ascending.
    active: Vec<u32>,
    wheel: BTreeMap<SimTime, Vec<Key>>,
    next_step: Vec<Key>,
    read_write: Vec<Key>,
    read_only: Vec<Key>,
    lists: Slab<WaitList>,
}

fn remove_key(v: &mut Vec<Key>, key: Key) {
    if let Some(i) = v.iter().position(|&k| k == key) {
        v.remove(i);
    }
}

impl WaitTable {
    pub(crate) fn new(slots: usize) -> Self {
        Self {
            seq: 0,
            waiters: Slab::default(),
            watch: (0..slots).map(|_| SlotWatch::default()).collect(),
            active: Vec::new(),
            wheel: BTreeMap::new(),
            next_step: Vec::new(),
            read_write: Vec::new(),
            read_only: Vec::new(),
            lists: Slab::default(),
        }
    }

    fn add(&mut self, waiter: NewWaiter, model: &Models) -> Key {
        self.seq += 1;
        let NewWaiter {
            on,
            task,
            waker,
            at,
        } = waiter;
        let key = self.waiters.insert(Waiter {
            task,
            waker,
            fired: false,
            seq: self.seq,
            on,
            at,
        });
        match on {
            WaitOn::Edge { slot, .. } => {
                let watch = &mut self.watch[slot as usize];
                if watch.waiters.is_empty() {
                    watch.snapshot.resize(words(model.slot_width(slot)), 0);
                    model.read(slot, &mut watch.snapshot);
                    if let Err(i) = self.active.binary_search(&slot) {
                        self.active.insert(i, slot);
                    }
                }
                watch.waiters.push(key);
            }
            WaitOn::Time(t) => self.wheel.entry(t).or_default().push(key),
            WaitOn::NextTimeStep => self.next_step.push(key),
            WaitOn::ReadWrite => self.read_write.push(key),
            WaitOn::ReadOnly => self.read_only.push(key),
            WaitOn::List(list) => {
                self.lists
                    .get_mut(list)
                    .or_invariant("invariant: a registration's wait list is alive")
                    .waiters
                    .push(key);
            }
        }
        key
    }

    fn unwatch_if_idle(&mut self, slot: u32) {
        if self.watch[slot as usize].waiters.is_empty()
            && let Ok(i) = self.active.binary_search(&slot)
        {
            self.active.remove(i);
        }
    }

    /// Removes a registration from its container (if still there) and from the table.
    fn remove(&mut self, key: Key) {
        let Some(w) = self.waiters.remove(key) else {
            return;
        };
        if w.fired {
            return;
        }
        match w.on {
            WaitOn::Edge { slot, .. } => {
                remove_key(&mut self.watch[slot as usize].waiters, key);
                self.unwatch_if_idle(slot);
            }
            WaitOn::Time(t) => {
                if let Some(v) = self.wheel.get_mut(&t) {
                    remove_key(v, key);
                    if v.is_empty() {
                        self.wheel.remove(&t);
                    }
                }
            }
            WaitOn::NextTimeStep => remove_key(&mut self.next_step, key),
            WaitOn::ReadWrite => remove_key(&mut self.read_write, key),
            WaitOn::ReadOnly => remove_key(&mut self.read_only, key),
            WaitOn::List(list) => {
                if let Some(l) = self.lists.get_mut(list) {
                    remove_key(&mut l.waiters, key);
                }
            }
        }
    }

    fn fire(&mut self, key: Key, out: &mut Vec<Wake>) {
        if let Some(w) = self.waiters.get_mut(key) {
            w.fired = true;
            out.push((w.seq, w.waker.clone()));
        }
    }

    fn fire_all(&mut self, keys: Vec<Key>, out: &mut Vec<Wake>) {
        for key in keys {
            self.fire(key, out);
        }
    }

    /// Fires timers due at `t`.
    pub(crate) fn fire_time(&mut self, t: SimTime, out: &mut Vec<Wake>) {
        let due = self.wheel.remove(&t).unwrap_or_default();
        self.fire_all(due, out);
    }

    pub(crate) fn fire_next_step(&mut self, out: &mut Vec<Wake>) {
        let keys = std::mem::take(&mut self.next_step);
        self.fire_all(keys, out);
    }

    pub(crate) fn fire_read_write(&mut self, out: &mut Vec<Wake>) {
        let keys = std::mem::take(&mut self.read_write);
        self.fire_all(keys, out);
    }

    /// The task and trigger location of every pending `ReadWrite` waiter, in
    /// registration order.
    pub(crate) fn read_write_waiters(
        &self,
    ) -> Vec<(Option<Key>, Option<&'static Location<'static>>)> {
        self.read_write
            .iter()
            .filter_map(|&key| self.waiters.get(key))
            .map(|w| (w.task, w.at))
            .collect()
    }

    pub(crate) fn fire_read_only(&mut self, out: &mut Vec<Wake>) {
        let keys = std::mem::take(&mut self.read_only);
        self.fire_all(keys, out);
    }

    pub(crate) fn next_timer(&self) -> Option<SimTime> {
        self.wheel.keys().next().copied()
    }

    /// Compares watched slots with their snapshot and fires matching waiters.
    pub(crate) fn compare(&mut self, model: &Models, out: &mut Vec<Wake>) {
        let mut scratch = Vec::new();
        for slot in self.active.clone() {
            let watch = &mut self.watch[slot as usize];
            scratch.resize(watch.snapshot.len(), 0);
            model.read(slot, &mut scratch);
            if scratch == watch.snapshot {
                continue;
            }
            let (old, new) = (watch.snapshot[0] & 1, scratch[0] & 1);
            watch.snapshot.copy_from_slice(&scratch);
            let mut fired = Vec::new();
            watch.waiters.retain(|&key| {
                let Some(WaitOn::Edge { edge, .. }) = self.waiters.get(key).map(|w| w.on) else {
                    return false;
                };
                let hit = match edge {
                    EdgeKind::Changed => true,
                    EdgeKind::Rising => old == 0 && new == 1,
                    EdgeKind::Falling => old == 1 && new == 0,
                };
                if hit {
                    fired.push(key);
                }
                !hit
            });
            self.fire_all(fired, out);
            self.unwatch_if_idle(slot);
        }
    }

    pub(crate) fn alloc_list(&mut self, kind: ListKind, name: Rc<str>) -> Key {
        self.lists.insert(WaitList {
            kind,
            name,
            waiters: Vec::new(),
        })
    }

    pub(crate) fn free_list(&mut self, list: Key) {
        if let Some(l) = self.lists.remove(list) {
            // Registrations still pointing at the list stay in the table until their
            // futures drop; they can no longer fire.
            debug_assert!(l.waiters.is_empty() || l.kind == ListKind::Task);
        }
    }

    pub(crate) fn list_has_waiters(&self, list: Key) -> bool {
        self.lists.get(list).is_some_and(|l| !l.waiters.is_empty())
    }

    fn fire_list(&mut self, list: Key, one: bool, out: &mut Vec<Wake>) {
        let Some(l) = self.lists.get_mut(list) else {
            return;
        };
        let keys = if one {
            if l.waiters.is_empty() {
                return;
            }
            vec![l.waiters.remove(0)]
        } else {
            std::mem::take(&mut l.waiters)
        };
        self.fire_all(keys, out);
    }

    pub(crate) fn live_waiters(&self) -> usize {
        self.waiters.len()
    }

    pub(crate) fn live_lists(&self) -> usize {
        self.lists.len()
    }

    /// What each unfired registration of `task` waits for, in registration order.
    pub(crate) fn describe(
        &self,
        task: Key,
        names: &[Box<str>],
        precision: Precision,
    ) -> Vec<String> {
        let mut found: Vec<(u64, String)> = self
            .waiters
            .iter()
            .filter(|(_, w)| w.task == Some(task) && !w.fired)
            .map(|(_, w)| (w.seq, self.describe_on(w.on, names, precision)))
            .collect();
        found.sort_by_key(|(seq, _)| *seq);
        found.into_iter().map(|(_, d)| d).collect()
    }

    /// Whether every unfired registration of `task` is on a wait list (a task, event, lock
    /// or queue), and there is at least one.
    pub(crate) fn only_lists(&self, task: Key) -> bool {
        let mut any = false;
        for (_, w) in self.waiters.iter() {
            if w.task == Some(task) && !w.fired {
                any = true;
                if !matches!(w.on, WaitOn::List(_)) {
                    return false;
                }
            }
        }
        any
    }

    fn describe_on(&self, on: WaitOn, names: &[Box<str>], precision: Precision) -> String {
        match on {
            WaitOn::Edge { slot, edge } => {
                let f = match edge {
                    EdgeKind::Rising => "rising",
                    EdgeKind::Falling => "falling",
                    EdgeKind::Changed => "changed",
                };
                format!("{f}(`{}`)", names[slot as usize])
            }
            WaitOn::Time(t) => format!("Timer until t={}", t.display(precision)),
            WaitOn::NextTimeStep => "NextTimeStep".to_owned(),
            WaitOn::ReadWrite => "ReadWrite".to_owned(),
            WaitOn::ReadOnly => "ReadOnly".to_owned(),
            WaitOn::List(list) => match self.lists.get(list) {
                Some(l) => {
                    let kind = match l.kind {
                        ListKind::Task => "Task",
                        ListKind::Event => "Event",
                        ListKind::Lock => "Lock",
                        ListKind::QueueGet => "Queue get on",
                        ListKind::QueuePut => "Queue put on",
                    };
                    format!("{kind} `{}`", l.name)
                }
                None => "a dropped wait list".to_owned(),
            },
        }
    }
}

/// Wakes collected wakers in registration order. No kernel borrow may be held: a waker
/// may be a foreign combinator's, running arbitrary code.
pub(crate) fn wake_sorted(mut wakes: Vec<Wake>) {
    wakes.sort_by_key(|(seq, _)| *seq);
    for (_, waker) in wakes {
        waker.wake();
    }
}

/// Fires every waiter on `list`.
pub(crate) fn wake_all(k: &Kernel, list: Key) {
    let mut out = Vec::new();
    k.waits.borrow_mut().fire_list(list, false, &mut out);
    wake_sorted(out);
}

/// Fires the oldest waiter on `list`; returns whether there was one.
pub(crate) fn wake_one(k: &Kernel, list: Key) -> bool {
    let mut out = Vec::new();
    k.waits.borrow_mut().fire_list(list, true, &mut out);
    let any = !out.is_empty();
    wake_sorted(out);
    any
}

/// A trigger's registration in the wait table; dropping it deregisters.
#[derive(Debug)]
pub(crate) struct Registration {
    sim_gen: u32,
    key: Key,
}

impl Registration {
    /// Registers the current task as waiting on `on`, to be woken through `cx`'s waker;
    /// `at` is where the trigger was made, if known.
    pub(crate) fn new(
        k: &Kernel,
        on: WaitOn,
        cx: &Context<'_>,
        at: Option<&'static Location<'static>>,
    ) -> Self {
        let model = k.models.borrow();
        let waiter = NewWaiter {
            on,
            task: k.current.get(),
            waker: cx.waker().clone(),
            at,
        };
        let key = k.waits.borrow_mut().add(waiter, &model);
        Self {
            sim_gen: k.sim_gen,
            key,
        }
    }

    /// Whether the registration has fired; if not, makes sure `cx`'s waker is the one woken.
    pub(crate) fn poll_fired(&self, cx: &Context<'_>) -> bool {
        state::with_sim(self.sim_gen, |k| {
            let mut waits = k.waits.borrow_mut();
            let Some(w) = waits.waiters.get_mut(self.key) else {
                return false;
            };
            if !w.fired && !w.waker.will_wake(cx.waker()) {
                w.waker.clone_from(cx.waker());
            }
            w.fired
        })
        .unwrap_or(false)
    }

    pub(crate) fn is_fired(&self) -> bool {
        state::with_sim(self.sim_gen, |k| {
            k.waits
                .borrow()
                .waiters
                .get(self.key)
                .is_some_and(|w| w.fired)
        })
        .unwrap_or(false)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        state::with_sim(self.sim_gen, |k| k.waits.borrow_mut().remove(self.key));
    }
}
