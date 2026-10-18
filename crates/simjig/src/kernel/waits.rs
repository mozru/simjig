//! The wait table: every pending trigger registration, and the containers that order them.
//!
//! A trigger future registers a [`Registration`] on first poll and holds it until it
//! completes; the registration's `Drop` removes it, so a dropped (cancelled) trigger never
//! leaves a dangling wake. Containers are plain vectors and a `BTreeMap` time wheel, never
//! a `HashMap`, so iteration order is deterministic.
//!
//! Firing marks a registration as fired, removes it from its container and wakes its
//! waker. Wakes that happen together (all `ReadWrite` waiters, one wait list) are issued in
//! registration order (`seq`).
//!
//! # Value-change groups (cocotb 2.1 on Verilator 5.052)
//! Edge waiters are grouped by (slot, [`EdgeKind`]), as cocotb keeps one trigger, and so
//! one VPI value-change callback, per signal and edge kind. A group is *primed* by the
//! registration that finds it empty: that registration's `seq` orders the group, and the
//! group snapshots the slot's value then (Verilator copies the value when the callback is
//! registered, `VerilatedVpiCbHolder`). Later waiters on the same (slot, kind) join the
//! group. It is unprimed when it fires (all its waiters wake together) or when its last
//! waiter deregisters; the next waiter primes a new group, last in the order.
//!
//! After each eval and each application of the write log, the scheduler walks the groups
//! that were primed before the walk began, oldest first ([`WaitTable::fire_next_change`]),
//! as Verilator's `callValueCbs` walks its callback list: a group whose slot differs from
//! its snapshot takes the new value, and fires if the change is its kind of edge. The
//! scheduler runs every task each firing makes ready before looking at the next group, so a
//! task woken by one group that waits on a younger group not yet reached joins it and wakes
//! in the same walk, while a group it primes itself (or re-primes after it fired) waits for
//! the next change. `changed`, `rising` and `falling` waiters on one signal are three groups.

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

impl EdgeKind {
    const fn index(self) -> usize {
        match self {
            Self::Rising => 0,
            Self::Falling => 1,
            Self::Changed => 2,
        }
    }
}

/// The waiters of one (slot, edge kind): one cocotb trigger and its VPI callback.
struct Group {
    /// The `seq` of the registration that primed it; groups fire in this order.
    primed: u64,
    slot: u32,
    edge: EdgeKind,
    /// The slot's value when this group last looked at it (or when it was primed).
    snapshot: Vec<u32>,
    /// In registration order.
    waiters: Vec<Key>,
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
    /// Registrations removed before they fired (cancelled waits), ever.
    cancelled: u64,
    waiters: Slab<Waiter>,
    /// Primed value-change groups, ascending by `primed`.
    groups: Vec<Group>,
    /// `group_of[slot][kind.index()]`: the `primed` of that slot's live group of that kind.
    group_of: Vec<[Option<u64>; 3]>,
    /// Snapshot buffers of unprimed groups, for reuse.
    spare: Vec<Vec<u32>>,
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
            cancelled: 0,
            waiters: Slab::default(),
            groups: Vec::new(),
            group_of: vec![[None; 3]; slots],
            spare: Vec::new(),
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
            WaitOn::Edge { slot, edge } => self.join_group(slot, edge, key, model),
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

    /// Adds `key` to the live group of (`slot`, `edge`), priming one if there is none.
    fn join_group(&mut self, slot: u32, edge: EdgeKind, key: Key, model: &Models) {
        let primed = &mut self.group_of[slot as usize][edge.index()];
        if let Some(seq) = *primed {
            let i = group_index(&self.groups, seq);
            self.groups[i].waiters.push(key);
            return;
        }
        *primed = Some(self.seq);
        let mut snapshot = self.spare.pop().unwrap_or_default();
        snapshot.resize(words(model.slot_width(slot)), 0);
        model.read(slot, &mut snapshot);
        // `seq` only grows, so pushing keeps `groups` sorted by `primed`.
        self.groups.push(Group {
            primed: self.seq,
            slot,
            edge,
            snapshot,
            waiters: vec![key],
        });
    }

    /// Unprimes the group at index `i` and returns its waiters.
    fn unprime(&mut self, i: usize) -> Vec<Key> {
        let g = self.groups.remove(i);
        self.group_of[g.slot as usize][g.edge.index()] = None;
        self.spare.push(g.snapshot);
        g.waiters
    }

    /// Removes a registration from its container (if still there) and from the table.
    fn remove(&mut self, key: Key) {
        let Some(w) = self.waiters.remove(key) else {
            return;
        };
        if w.fired {
            return;
        }
        self.cancelled += 1;
        match w.on {
            WaitOn::Edge { slot, edge } => {
                if let Some(seq) = self.group_of[slot as usize][edge.index()] {
                    let i = group_index(&self.groups, seq);
                    remove_key(&mut self.groups[i].waiters, key);
                    if self.groups[i].waiters.is_empty() {
                        self.unprime(i);
                    }
                }
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

    /// Fires the oldest timer due at `t`; returns whether there was one.
    ///
    /// One at a time, because cocotb runs every task a Timer callback wakes, and every task
    /// those wake, before the next Timer callback: see `sched::step`.
    pub(crate) fn fire_next_timer(&mut self, t: SimTime, out: &mut Vec<Wake>) -> bool {
        let Some(due) = self.wheel.get_mut(&t) else {
            return false;
        };
        // Registration order: a timer is appended when registered, so the oldest is first.
        let key = due.remove(0);
        if due.is_empty() {
            self.wheel.remove(&t);
        }
        self.fire(key, out);
        true
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

    /// The `seq` of the newest registration: groups primed after a walk starts have a
    /// larger one and are not part of that walk.
    pub(crate) fn last_seq(&self) -> u64 {
        self.seq
    }

    /// One step of a value-change walk: looks at the groups primed after `*after` and no
    /// later than `until`, oldest first, until one fires. Each group whose slot differs
    /// from its snapshot takes the new value; a group whose edge that is fires all its
    /// waiters, in registration order, and is unprimed. Returns whether one fired; `*after`
    /// is where the next step resumes.
    pub(crate) fn fire_next_change(
        &mut self,
        model: &Models,
        until: u64,
        after: &mut u64,
        out: &mut Vec<Wake>,
    ) -> bool {
        let mut i = self.groups.partition_point(|g| g.primed <= *after);
        let mut scratch = Vec::new();
        while let Some(g) = self.groups.get_mut(i).filter(|g| g.primed <= until) {
            *after = g.primed;
            scratch.resize(g.snapshot.len(), 0);
            model.read(g.slot, &mut scratch);
            if scratch != g.snapshot {
                let (old, new) = (g.snapshot[0] & 1, scratch[0] & 1);
                g.snapshot.copy_from_slice(&scratch);
                let hit = match g.edge {
                    EdgeKind::Changed => true,
                    EdgeKind::Rising => old == 0 && new == 1,
                    EdgeKind::Falling => old == 1 && new == 0,
                };
                if hit {
                    let waiters = self.unprime(i);
                    self.fire_all(waiters, out);
                    return true;
                }
            }
            i += 1;
        }
        *after = until;
        false
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

/// The index of the group primed by registration `seq`.
fn group_index(groups: &[Group], seq: u64) -> usize {
    groups
        .binary_search_by_key(&seq, |g| g.primed)
        .ok()
        .or_invariant("invariant: a primed group is in the group list")
}

/// Wakes collected wakers in registration order. No kernel borrow may be held: a waker
/// may be a foreign combinator's, running arbitrary code.
pub(crate) fn wake_sorted(mut wakes: Vec<Wake>) {
    wakes.sort_by_key(|(seq, _)| *seq);
    for (_, waker) in wakes {
        waker.wake();
    }
}

/// How many registrations of the current simulation were removed before they fired; 0
/// outside a simulation. A combinator compares it around dropping its losers to learn
/// whether one was still waiting (see `combinator::hop`).
pub(crate) fn cancelled_waits() -> u64 {
    state::try_current("a combinator", |k| k.waits.borrow().cancelled).unwrap_or(0)
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
