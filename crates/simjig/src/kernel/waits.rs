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

use std::collections::{BTreeMap, VecDeque};
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
/// Keyed in [`WaitTable::groups`] by the `seq` of the registration that primed it; groups
/// fire in that order.
struct Group {
    slot: u32,
    edge: EdgeKind,
    /// The slot's value when this group last looked at it (or when it was primed).
    snapshot: Vec<u32>,
    /// In registration order.
    waiters: Pending,
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
    /// Primed value-change groups by `primed`: a walk visits them in key order, and
    /// priming, firing and unpriming one is O(log G).
    groups: BTreeMap<u64, Group>,
    /// `group_of[slot][kind.index()]`: the `primed` of that slot's live group of that kind.
    group_of: Vec<[Option<u64>; 3]>,
    /// Snapshot buffers of unprimed groups, for reuse.
    spare: Vec<Vec<u32>>,
    /// The value being compared, kept to avoid an allocation per walk step.
    scratch: Vec<u32>,
    /// The wake buffer [`fire_and_wake`] lends out; empty between calls.
    wakes: Vec<Wake>,
    wheel: BTreeMap<SimTime, Pending>,
    next_step: Vec<Key>,
    read_write: Vec<Key>,
    read_only: Vec<Key>,
    lists: Slab<WaitList>,
}

/// Registrations of one group or one timer deadline, in registration order, removed
/// lazily: a cancelled registration leaves the slab at once and its stale key here is
/// skipped (and compacted away once most keys are stale). Cancelling one of many waiters
/// is then O(1), not a search and shift; a group or deadline with nobody left is still
/// removed at once.
#[derive(Default)]
struct Pending {
    keys: VecDeque<Key>,
    live: usize,
}

impl Pending {
    fn one(key: Key) -> Self {
        let mut p = Self::default();
        p.push(key);
        p
    }

    fn push(&mut self, key: Key) {
        self.keys.push_back(key);
        self.live += 1;
    }

    /// Notes that one of its registrations left `waiters`; returns whether none is left.
    fn forget(&mut self, waiters: &Slab<Waiter>) -> bool {
        self.live -= 1;
        if self.live == 0 {
            self.keys.clear();
            return true;
        }
        if self.keys.len() > 2 * self.live + 16 {
            self.keys.retain(|&k| waiters.get(k).is_some());
        }
        false
    }

    /// Takes the oldest live registration.
    fn pop(&mut self, waiters: &Slab<Waiter>) -> Option<Key> {
        while let Some(key) = self.keys.pop_front() {
            if waiters.get(key).is_some() {
                self.live -= 1;
                return Some(key);
            }
        }
        None
    }
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
            groups: BTreeMap::new(),
            group_of: vec![[None; 3]; slots],
            spare: Vec::new(),
            scratch: Vec::new(),
            wakes: Vec::new(),
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
            group(&mut self.groups, seq).waiters.push(key);
            return;
        }
        *primed = Some(self.seq);
        let mut snapshot = self.spare.pop().unwrap_or_default();
        snapshot.resize(words(model.slot_width(slot)), 0);
        model.read(slot, &mut snapshot);
        self.groups.insert(
            self.seq,
            Group {
                slot,
                edge,
                snapshot,
                waiters: Pending::one(key),
            },
        );
    }

    /// Unprimes the group primed by `primed` and returns its waiters.
    fn unprime(&mut self, primed: u64) -> VecDeque<Key> {
        let g = self
            .groups
            .remove(&primed)
            .or_invariant("invariant: a primed group is in the group map");
        self.group_of[g.slot as usize][g.edge.index()] = None;
        self.spare.push(g.snapshot);
        g.waiters.keys
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
            WaitOn::Edge { slot, edge } => {
                if let Some(seq) = self.group_of[slot as usize][edge.index()]
                    && group(&mut self.groups, seq).waiters.forget(&self.waiters)
                {
                    self.unprime(seq);
                }
            }
            WaitOn::Time(t) => {
                if let Some(due) = self.wheel.get_mut(&t)
                    && due.forget(&self.waiters)
                {
                    self.wheel.remove(&t);
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

    /// Fires `keys`, skipping those whose registration was removed.
    fn fire_all(&mut self, keys: impl IntoIterator<Item = Key>, out: &mut Vec<Wake>) {
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
        let key = due
            .pop(&self.waiters)
            .or_invariant("invariant: a deadline in the wheel has a live timer");
        if due.live == 0 {
            self.wheel.remove(&t);
        }
        self.fire(key, out);
        true
    }

    /// Fires every key of `keys`, emptying it but keeping its capacity.
    fn fire_drain(&mut self, keys: fn(&mut Self) -> &mut Vec<Key>, out: &mut Vec<Wake>) {
        if keys(self).is_empty() {
            return;
        }
        let mut taken = std::mem::take(keys(self));
        self.fire_all(taken.drain(..), out);
        // Firing registers nothing, so the list is still empty: give it its buffer back.
        *keys(self) = taken;
    }

    pub(crate) fn fire_next_step(&mut self, out: &mut Vec<Wake>) {
        self.fire_drain(|w| &mut w.next_step, out);
    }

    pub(crate) fn fire_read_write(&mut self, out: &mut Vec<Wake>) {
        self.fire_drain(|w| &mut w.read_write, out);
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
        self.fire_drain(|w| &mut w.read_only, out);
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
    ///
    /// Edges compare bit 0 of the old and new value. Edge triggers exist only on 1-bit
    /// signals (`trigger::edge` rejects wider ones, as cocotb's `rising_edge` raises
    /// `TypeError` for a multi-bit signal), so bit 0 is the whole value.
    ///
    /// `until` mirrors `callValueCbs`, which skips callbacks added during the walk. With
    /// writes held in the write log, a group primed during the walk snapshots the value
    /// already changed and could not fire anyway; the bound keeps that a property of the
    /// walk rather than of the write log, and spares looking at such groups.
    pub(crate) fn fire_next_change(
        &mut self,
        model: &Models,
        until: u64,
        after: &mut u64,
        out: &mut Vec<Wake>,
    ) -> bool {
        if *after >= until {
            return false;
        }
        let scratch = &mut self.scratch;
        let mut hit = None;
        for (&primed, g) in self.groups.range_mut(*after + 1..=until) {
            *after = primed;
            scratch.resize(g.snapshot.len(), 0);
            model.read(g.slot, scratch);
            if *scratch == g.snapshot {
                continue;
            }
            let (old, new) = (g.snapshot[0] & 1, scratch[0] & 1);
            g.snapshot.copy_from_slice(scratch);
            let fires = match g.edge {
                EdgeKind::Changed => true,
                EdgeKind::Rising => old == 0 && new == 1,
                EdgeKind::Falling => old == 1 && new == 0,
            };
            if fires {
                hit = Some(primed);
                break;
            }
        }
        let Some(primed) = hit else {
            *after = until;
            return false;
        };
        let waiters = self.unprime(primed);
        self.fire_all(waiters, out);
        true
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
        if one {
            if l.waiters.is_empty() {
                return;
            }
            let key = l.waiters.remove(0);
            self.fire(key, out);
        } else {
            // Taken and given back, so the list keeps its capacity; firing registers
            // nothing, so it is still empty when it gets the buffer back.
            let mut keys = std::mem::take(&mut l.waiters);
            self.fire_all(keys.drain(..), out);
            if let Some(l) = self.lists.get_mut(list) {
                l.waiters = keys;
            }
        }
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

/// The group primed by registration `seq`.
fn group(groups: &mut BTreeMap<u64, Group>, seq: u64) -> &mut Group {
    groups
        .get_mut(&seq)
        .or_invariant("invariant: a primed group is in the group map")
}

/// Wakes collected wakers in registration order. No kernel borrow may be held: a waker
/// may be a foreign combinator's, running arbitrary code.
///
/// A foreign waker that panics is not caught here. Woken by the scheduler loop (a value
/// change, a Timer, a phase), which does not run under `catch_unwind`, the panic escapes
/// [`Sim::run`](crate::Sim::run) as a panic, not as a [`Failure`](crate::Failure); woken
/// from inside a task (an `Event` it sets), it becomes that task's `TaskPanic`.
fn wake_sorted(wakes: &mut Vec<Wake>) {
    wakes.sort_by_key(|(seq, _)| *seq);
    for (_, waker) in wakes.drain(..) {
        waker.wake();
    }
}

/// Runs `fire` on the wait table, then wakes what it fired with [`wake_sorted`], after the
/// table's borrow ends.
///
/// The wake buffer is the table's, lent out for the call and given back, so a timestep
/// allocates none, and a call that fired nothing costs one swap. A waker that fires again
/// before the outer wake is done (a foreign one can) borrows the empty buffer left behind;
/// whichever buffer is larger is kept.
pub(crate) fn fire_and_wake<R>(
    k: &Kernel,
    fire: impl FnOnce(&mut WaitTable, &mut Vec<Wake>) -> R,
) -> R {
    let (r, mut wakes) = {
        let mut waits = k.waits.borrow_mut();
        let mut wakes = std::mem::take(&mut waits.wakes);
        let r = fire(&mut waits, &mut wakes);
        if wakes.is_empty() {
            waits.wakes = wakes;
            return r;
        }
        (r, wakes)
    };
    wake_sorted(&mut wakes);
    let mut waits = k.waits.borrow_mut();
    if wakes.capacity() > waits.wakes.capacity() {
        waits.wakes = wakes;
    }
    r
}

/// Fires every waiter on `list`.
pub(crate) fn wake_all(k: &Kernel, list: Key) {
    fire_and_wake(k, |w, out| w.fire_list(list, false, out));
}

/// Fires the oldest waiter on `list`; returns whether there was one.
pub(crate) fn wake_one(k: &Kernel, list: Key) -> bool {
    fire_and_wake(k, |w, out| {
        w.fire_list(list, true, out);
        !out.is_empty()
    })
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
