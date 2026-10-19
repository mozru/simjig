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
///
/// Made on the first wait on its (slot, kind) and kept for the simulation's lifetime, primed
/// and unprimed in place, so priming it again reuses its snapshot and key buffers: a clock
/// edge waited on every cycle allocates nothing. Groups fire in the order of `primed`.
struct Group {
    slot: u32,
    edge: EdgeKind,
    /// The `seq` of the registration that primed it; `None` while unprimed.
    primed: Option<u64>,
    /// The slot's word count.
    words: usize,
    /// The slot's value when this group last looked at it (or when it was primed): here
    /// for a slot of at most two words (compared as one array, the common case), else in
    /// `wide`.
    narrow: [u32; 2],
    wide: Vec<u32>,
    /// In registration order; empty while unprimed.
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

/// `group_of` entry of a (slot, kind) that has no group yet.
const NO_GROUP: u32 = u32::MAX;

pub(crate) struct WaitTable {
    seq: u64,
    waiters: Slab<Waiter>,
    /// Every group made so far, primed or not; see [`Group`].
    groups: Vec<Group>,
    /// `group_of[slot][kind.index()]`: the index in `groups` of that slot's group of that
    /// kind, or [`NO_GROUP`].
    group_of: Vec<[u32; 3]>,
    /// `(primed, group index)` of every priming, in priming order (`primed` only grows, so
    /// this is sorted): a walk visits the primed groups in this order. Unpriming leaves its
    /// entry behind, stale (the group's `primed` no longer matches); `stale` counts them.
    /// Stale entries at the front are skipped at once by moving `head` past them (the
    /// group that fires is usually the oldest), and dropped once they are most of the
    /// list; stale entries elsewhere are compacted away once they outnumber the live ones,
    /// so a walk seldom steps over one. Priming, firing and unpriming are O(1) amortized, and a walk finds
    /// its start by binary search.
    order: Vec<(u64, u32)>,
    /// `order[..head]` is stale and skipped (dropping it from a `Vec` would move the rest).
    head: usize,
    stale: usize,
    /// Key buffers of timer deadlines that left the wheel, for reuse.
    spare_keys: Vec<VecDeque<Key>>,
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
    /// An empty one, in `keys` if given (a buffer kept for reuse).
    fn reusing(keys: Option<VecDeque<Key>>) -> Self {
        Self {
            keys: keys.unwrap_or_default(),
            live: 0,
        }
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
            groups: Vec::new(),
            group_of: vec![[NO_GROUP; 3]; slots],
            order: Vec::new(),
            head: 0,
            stale: 0,
            spare_keys: Vec::new(),
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
            WaitOn::Time(t) => {
                let spare = &mut self.spare_keys;
                self.wheel
                    .entry(t)
                    .or_insert_with(|| Pending::reusing(spare.pop()))
                    .push(key);
            }
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

    /// Adds `key` to the group of (`slot`, `edge`), priming it if it is not primed.
    fn join_group(&mut self, slot: u32, edge: EdgeKind, key: Key, model: &Models) {
        let cell = &mut self.group_of[slot as usize][edge.index()];
        if *cell == NO_GROUP {
            *cell = u32::try_from(self.groups.len()).or_invariant("invariant: < 2^32 groups");
            self.groups.push(Group {
                slot,
                edge,
                primed: None,
                words: words(model.slot_width(slot)),
                narrow: [0; 2],
                wide: Vec::new(),
                waiters: Pending::default(),
            });
        }
        let index = *cell;
        let g = &mut self.groups[index as usize];
        g.waiters.push(key);
        if g.primed.is_some() {
            return;
        }
        g.primed = Some(self.seq);
        if g.words <= 2 {
            g.narrow = [0; 2];
            model.read(slot, &mut g.narrow[..g.words]);
        } else {
            g.wide.resize(g.words, 0);
            model.read(slot, &mut g.wide);
        }
        self.order.push((self.seq, index));
    }

    /// Unprimes group `index`, leaving its entry in `order` stale; its waiters must already
    /// be gone (fired or forgotten).
    fn unprime(&mut self, index: u32) {
        let g = &mut self.groups[index as usize];
        g.primed = None;
        g.waiters.keys.clear();
        g.waiters.live = 0;
        self.stale += 1;
        let groups = &self.groups;
        let live = |&(seq, i): &(u64, u32)| groups[i as usize].primed == Some(seq);
        while self.order.get(self.head).is_some_and(|e| !live(e)) {
            self.head += 1;
            self.stale -= 1;
        }
        let kept = self.order.len() - self.head;
        if self.head > 16 && self.head > kept {
            self.order.drain(..self.head);
            self.head = 0;
        }
        if self.stale > 4 && 2 * self.stale > kept {
            self.order.drain(..self.head);
            self.head = 0;
            self.order.retain(live);
            self.stale = 0;
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
            WaitOn::Edge { slot, edge } => {
                // An unfired registration is in its (slot, kind)'s group, which is primed.
                let index = self.group_of[slot as usize][edge.index()];
                if self.groups[index as usize].waiters.forget(&self.waiters) {
                    self.unprime(index);
                }
            }
            WaitOn::Time(t) => {
                if let Some(due) = self.wheel.get_mut(&t)
                    && due.forget(&self.waiters)
                {
                    self.retire_deadline(t);
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
            self.retire_deadline(t);
        }
        self.fire(key, out);
        true
    }

    /// Removes deadline `t`, which has no live timer left, keeping its key buffer.
    fn retire_deadline(&mut self, t: SimTime) {
        if let Some(mut due) = self.wheel.remove(&t) {
            due.keys.clear();
            self.spare_keys.push(due.keys);
        }
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

    /// Whether anything waits on the region `on` (`NextTimeStep`, `ReadWrite` or
    /// `ReadOnly`); the scheduler skips firing an empty region.
    pub(crate) fn region_waited(&self, on: WaitOn) -> bool {
        match on {
            WaitOn::NextTimeStep => !self.next_step.is_empty(),
            WaitOn::ReadWrite => !self.read_write.is_empty(),
            WaitOn::ReadOnly => !self.read_only.is_empty(),
            WaitOn::Edge { .. } | WaitOn::Time(_) | WaitOn::List(_) => {
                unreachable!("invariant: only regions are asked")
            }
        }
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
        let live = &self.order[self.head..];
        let first = live.partition_point(|&(seq, _)| seq <= *after);
        let mut hit = None;
        for &(primed, index) in &live[first..] {
            if primed > until {
                break;
            }
            *after = primed;
            let g = &mut self.groups[index as usize];
            if g.primed != Some(primed) {
                continue; // stale: unprimed since
            }
            let (old, new) = if g.words <= 2 {
                let mut now = [0; 2];
                model.read(g.slot, &mut now[..g.words]);
                if now == g.narrow {
                    continue;
                }
                let old = g.narrow[0] & 1;
                g.narrow = now;
                (old, now[0] & 1)
            } else {
                self.scratch.resize(g.words, 0);
                model.read(g.slot, &mut self.scratch);
                if self.scratch == g.wide {
                    continue;
                }
                let old = g.wide[0] & 1;
                g.wide.copy_from_slice(&self.scratch);
                (old, self.scratch[0] & 1)
            };
            let fires = match g.edge {
                EdgeKind::Changed => true,
                EdgeKind::Rising => old == 0 && new == 1,
                EdgeKind::Falling => old == 1 && new == 0,
            };
            if fires {
                hit = Some(index);
                break;
            }
        }
        let Some(index) = hit else {
            *after = until;
            return false;
        };
        // Fired from the group's own buffer, which it keeps for its next priming.
        let mut keys = std::mem::take(&mut self.groups[index as usize].waiters.keys);
        self.fire_all(keys.drain(..), out);
        self.groups[index as usize].waiters.keys = keys;
        self.unprime(index);
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
