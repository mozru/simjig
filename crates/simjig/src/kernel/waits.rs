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
//! Edge registrations ([`EdgeRegistration`]) live in their group, in registration order,
//! not in the table's slab: firing moves the group's wakers out in that order, and a
//! registration has fired exactly when its group is no longer primed by the priming it
//! joined (a registration not yet cancelled keeps its group primed, so only firing can
//! unprime it). Registering, firing and seeing the fire are then a push, a drain and a
//! compare; nothing is looked up or freed per waiter.
//!
//! After each eval and each application of the write log, the scheduler walks the groups
//! that were primed before the walk began, oldest first ([`WaitTable::next_change`]),
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
use super::tasks::ReadyQueue;
use super::time::{Precision, SimTime};
use super::waker;

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
    /// While primed: the index of its [`Watch`] in `order`.
    at: usize,
    /// The slot's value when this group last looked at it (or when it was primed), for a
    /// slot of more than two words; a narrower slot's is in its [`Watch`].
    wide: Vec<u32>,
    /// Its registrations, in registration order (so sorted by `seq`); empty while unprimed.
    /// A cancelled one stays, without its waker, until the group fires or most are
    /// cancelled.
    waiters: Vec<EdgeWaiter>,
    /// The registrations in `waiters` not cancelled.
    live: usize,
}

/// One priming of a [`Group`], in `WaitTable::order`: what a walk step looks at, in one
/// place, so that a step over a group that did not change touches no group.
struct Watch {
    /// The `seq` of the registration that primed the group.
    primed: u64,
    /// The group, or [`STALE`] once it was unprimed.
    group: u32,
    /// The group's slot.
    slot: u32,
    /// The slot's value when the group last looked at it, for a slot of at most two words
    /// (as one word, low word first).
    snapshot: u64,
}

/// [`Watch::group`] of a priming that was undone.
const STALE: u32 = u32::MAX;

/// One registration of a [`Group`]. Edge registrations live in their group, not in the
/// table's slab: the group fires them all at once, in order, and a registration has fired
/// exactly when its group is no longer primed by the priming it joined
/// ([`EdgeRegistration`]).
struct EdgeWaiter {
    seq: u64,
    task: Option<Key>,
    wake: EdgeWake,
}

/// Whom an edge registration wakes when its group fires.
///
/// One of this simulation's tasks is kept as its slot index and generation, not as its
/// waker: queueing it is all its waker's `wake` would do, so its waker is neither cloned
/// when it registers nor dropped when it fires, and seeing that every waiter of a group is
/// such a task is a tag test.
enum EdgeWake {
    /// A task of this simulation (a waker made by `waker::task_waker` for it).
    Task(u32, u16),
    /// Any other waker.
    Foreign(Waker),
    /// Cancelled.
    Gone,
}

impl EdgeWake {
    /// Whom `waker` wakes, for simulation `sim_gen`.
    fn of(waker: &Waker, sim_gen: u32) -> Self {
        match waker::own_task(waker) {
            Some((g, index, gen16)) if g == sim_gen => Self::Task(index, gen16),
            _ => Self::Foreign(waker.clone()),
        }
    }

    /// Whether this wakes what `waker` wakes ([`Waker::will_wake`]).
    fn wakes_as(&self, waker: &Waker, sim_gen: u32) -> bool {
        match self {
            Self::Task(index, gen16) => waker::own_task(waker) == Some((sim_gen, *index, *gen16)),
            Self::Foreign(w) => w.will_wake(waker),
            Self::Gone => false,
        }
    }
}

/// What [`WaitTable::add`] and [`WaitTable::add_edge`] need to register a waiter.
pub(crate) struct NewWaiter {
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
    /// The simulation's id: a waker carrying it is one of its tasks' ([`waker::own_task`]).
    sim_gen: u32,
    seq: u64,
    /// Every registration but the edge ones, which are in their groups.
    waiters: Slab<Waiter>,
    /// Edge registrations not yet fired or cancelled, in all groups.
    edge_live: usize,
    /// Every group made so far, primed or not; see [`Group`].
    groups: Vec<Group>,
    /// `group_of[slot][kind.index()]`: the index in `groups` of that slot's group of that
    /// kind, or [`NO_GROUP`].
    group_of: Vec<[u32; 3]>,
    /// Every priming, in priming order (`primed` only grows, so this is sorted): a walk
    /// visits the primed groups in this order. Unpriming leaves its entry behind, marked
    /// [`STALE`]; `stale` counts them.
    /// Stale entries at the front are skipped at once by moving `head` past them (the
    /// group that fires is usually the oldest), and dropped once they are most of the
    /// list; stale entries elsewhere are compacted away once they outnumber the live ones,
    /// so a walk seldom steps over one. Priming, firing and unpriming are O(1) amortized, and a walk finds
    /// its start by binary search.
    order: Vec<Watch>,
    /// `order[..head]` is stale and skipped (dropping it from a `Vec` would move the rest).
    head: usize,
    stale: usize,
    /// A value-change walk is under way: `order` is not compacted until it ends, so the
    /// walk's place in it stays valid ([`next_change`](Self::next_change)).
    walking: bool,
    /// Key buffers of timer deadlines that left the wheel, for reuse.
    spare_keys: Vec<VecDeque<Key>>,
    /// The value being compared, kept to avoid an allocation per walk step.
    scratch: Vec<u32>,
    /// The wake buffer [`fire_and_wake`] lends out; empty between calls.
    wakes: Vec<Wake>,
    /// The wake buffer a value-change walk holds from [`begin_walk`](Self::begin_walk) to
    /// [`end_walk`](Self::end_walk) (tasks it wakes may use `wakes` meanwhile).
    walk_wakes: Vec<Wake>,
    wheel: BTreeMap<SimTime, Pending>,
    next_step: Vec<Key>,
    read_write: Vec<Key>,
    read_only: Vec<Key>,
    lists: Slab<WaitList>,
}

/// Registrations of one timer deadline, in registration order, removed lazily: a
/// cancelled registration leaves the slab at once and its stale key here is skipped (and
/// compacted away once most keys are stale). Cancelling one of many waiters is then O(1),
/// not a search and shift; a deadline with nobody left is still removed at once.
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
    /// A table for simulation `sim_gen` over `slots` flat slots.
    pub(crate) fn new(sim_gen: u32, slots: usize) -> Self {
        Self {
            sim_gen,
            seq: 0,
            waiters: Slab::default(),
            edge_live: 0,
            groups: Vec::new(),
            group_of: vec![[NO_GROUP; 3]; slots],
            order: Vec::new(),
            head: 0,
            stale: 0,
            walking: false,
            spare_keys: Vec::new(),
            scratch: Vec::new(),
            wakes: Vec::new(),
            walk_wakes: Vec::new(),
            wheel: BTreeMap::new(),
            next_step: Vec::new(),
            read_write: Vec::new(),
            read_only: Vec::new(),
            lists: Slab::default(),
        }
    }

    fn add(&mut self, waiter: NewWaiter) -> Key {
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
            WaitOn::Edge { .. } => unreachable!("invariant: edges register with add_edge"),
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

    /// Registers task `task`, woken through `waker`, as waiting on `edge` of `slot`, in the
    /// group of that (slot, kind), priming the group if it is not primed.
    pub(crate) fn add_edge(
        &mut self,
        slot: u32,
        edge: EdgeKind,
        task: Option<Key>,
        waker: &Waker,
        model: &Models,
    ) -> EdgeKey {
        let wake = EdgeWake::of(waker, self.sim_gen);
        self.seq += 1;
        let seq = self.seq;
        let cell = &mut self.group_of[slot as usize][edge.index()];
        if *cell == NO_GROUP {
            *cell = u32::try_from(self.groups.len()).or_invariant("invariant: < 2^32 groups");
            self.groups.push(Group {
                slot,
                edge,
                primed: None,
                words: words(model.slot_width(slot)),
                at: 0,
                wide: Vec::new(),
                waiters: Vec::new(),
                live: 0,
            });
        }
        let group = *cell;
        let g = &mut self.groups[group as usize];
        g.waiters.push(EdgeWaiter { seq, task, wake });
        g.live += 1;
        self.edge_live += 1;
        if let Some(primed) = g.primed {
            return EdgeKey { group, primed, seq };
        }
        g.primed = Some(seq);
        let snapshot = if g.words <= 2 {
            model.read_narrow(slot, g.words)
        } else {
            g.wide.resize(g.words, 0);
            model.read(slot, &mut g.wide);
            0
        };
        g.at = self.order.len();
        self.order.push(Watch {
            primed: seq,
            group,
            slot,
            snapshot,
        });
        EdgeKey {
            group,
            primed: seq,
            seq,
        }
    }

    /// Whether edge registration `key` has fired: its group fired since (and may have been
    /// primed again). A registration not yet cancelled keeps its group primed, so nothing
    /// else unprimes it.
    fn edge_fired(&self, key: EdgeKey) -> bool {
        self.groups[key.group as usize].primed != Some(key.primed)
    }

    /// The waiter of edge registration `key`, which has not fired.
    fn edge_waiter(&mut self, key: EdgeKey) -> Option<&mut EdgeWaiter> {
        let waiters = &mut self.groups[key.group as usize].waiters;
        let i = waiters.binary_search_by_key(&key.seq, |w| w.seq).ok()?;
        waiters.get_mut(i)
    }

    /// Cancels edge registration `key` if it has not fired, unpriming its group if it was
    /// the last.
    fn remove_edge(&mut self, key: EdgeKey) {
        if self.edge_fired(key) {
            return;
        }
        let Some(w) = self.edge_waiter(key) else {
            return;
        };
        if matches!(
            std::mem::replace(&mut w.wake, EdgeWake::Gone),
            EdgeWake::Gone
        ) {
            return;
        }
        w.task = None;
        self.edge_live -= 1;
        let g = &mut self.groups[key.group as usize];
        g.live -= 1;
        if g.live == 0 {
            self.unprime(key.group);
        } else if g.waiters.len() > 2 * g.live + 16 {
            g.waiters.retain(|w| !matches!(w.wake, EdgeWake::Gone));
        }
    }

    /// Unprimes group `index`, leaving its entry in `order` stale; its waiters must already
    /// be gone (fired or forgotten).
    fn unprime(&mut self, index: u32) {
        let g = &mut self.groups[index as usize];
        g.primed = None;
        g.waiters.clear();
        g.live = 0;
        self.order[g.at].group = STALE;
        self.stale += 1;
        while self.order.get(self.head).is_some_and(|w| w.group == STALE) {
            self.head += 1;
            self.stale -= 1;
        }
        if !self.walking {
            self.compact();
        }
    }

    /// Drops the stale entries of `order` once they are most of it (see `order`). After
    /// every walk and unpriming, and seldom with anything to do: the test is inlined.
    #[inline]
    fn compact(&mut self) {
        let kept = self.order.len() - self.head;
        if (self.head > 16 && self.head > kept) || (self.stale > 4 && 2 * self.stale > kept) {
            self.compact_now(kept);
        }
    }

    #[inline(never)]
    fn compact_now(&mut self, kept: usize) {
        if self.head > 16 && self.head > kept {
            self.order.drain(..self.head);
            self.head = 0;
        }
        if self.stale > 4 && 2 * self.stale > kept {
            self.order.drain(..self.head);
            self.head = 0;
            self.order.retain(|w| w.group != STALE);
            self.stale = 0;
        }
        // Entries moved: tell each primed group where its watch is now.
        for (i, w) in self.order.iter().enumerate().skip(self.head) {
            if w.group != STALE {
                self.groups[w.group as usize].at = i;
            }
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
            WaitOn::Edge { .. } => unreachable!("invariant: edge waits are in their groups"),
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

    /// The newest registration's `seq`: a walk's bound ([`next_change`](Self::next_change)).
    pub(crate) fn newest(&self) -> u64 {
        self.seq
    }

    /// Starts a value-change walk: until [`end_walk`](Self::end_walk), `order` keeps its
    /// places, so the walk resumes from where [`next_change`](Self::next_change) left
    /// `*at`. Returns the walk's wake buffer (empty).
    pub(crate) fn begin_walk(&mut self) -> Vec<Wake> {
        self.walking = true;
        std::mem::take(&mut self.walk_wakes)
    }

    /// Ends a walk begun with [`begin_walk`](Self::begin_walk), taking back its buffer.
    pub(crate) fn end_walk(&mut self, wakes: Vec<Wake>) {
        debug_assert!(wakes.is_empty(), "invariant: a walk wakes all it fires");
        self.walk_wakes = wakes;
        self.walking = false;
        self.compact();
    }

    /// One step of a value-change walk: looks at the groups primed no later than `until`
    /// from place `*at` of `order` on, oldest first, until one fires. Each group whose slot
    /// differs from its snapshot takes the new value. Returns the first group whose edge
    /// that is, for [`fire_group`](Self::fire_group); `*at` is where the next step resumes.
    ///
    /// Edges compare bit 0 of the old and new value. Edge triggers exist only on 1-bit
    /// signals (`trigger::edge` rejects wider ones, as cocotb's `rising_edge` raises
    /// `TypeError` for a multi-bit signal), so bit 0 is the whole value.
    ///
    /// `until` is an optimisation, not an ordering rule: it only spares looking at groups
    /// primed during the walk. Such a group snapshots the value when primed, and no value
    /// changes during a walk (writes wait in the write log, and no eval runs), so it could
    /// not fire in this walk anyway; removing the bound changes no order. The result is
    /// what `callValueCbs` does by skipping callbacks added during its walk.
    pub(crate) fn next_change(
        &mut self,
        model: &Models,
        until: u64,
        at: &mut usize,
    ) -> Option<u32> {
        let mut i = (*at).max(self.head);
        while let Some(w) = self.order.get_mut(i) {
            if w.primed > until {
                break;
            }
            i += 1;
            if w.group == STALE {
                continue; // unprimed since
            }
            let index = w.group;
            let (old, new) = match model.direct_u64(w.slot) {
                // The common case: a narrow slot with a direct way, no group looked at.
                Some(now) if now == w.snapshot => continue,
                Some(now) => (std::mem::replace(&mut w.snapshot, now) & 1, now & 1),
                None => {
                    let g = &mut self.groups[index as usize];
                    if g.words <= 2 {
                        let now = model.read_narrow(w.slot, g.words);
                        if now == w.snapshot {
                            continue;
                        }
                        (std::mem::replace(&mut w.snapshot, now) & 1, now & 1)
                    } else {
                        self.scratch.resize(g.words, 0);
                        model.read(g.slot, &mut self.scratch);
                        if self.scratch == g.wide {
                            continue;
                        }
                        let old = u64::from(g.wide[0] & 1);
                        g.wide.copy_from_slice(&self.scratch);
                        (old, u64::from(self.scratch[0] & 1))
                    }
                }
            };
            let fires = match self.groups[index as usize].edge {
                EdgeKind::Changed => true,
                EdgeKind::Rising => old == 0 && new == 1,
                EdgeKind::Falling => old == 1 && new == 0,
            };
            if fires {
                *at = i;
                return Some(index);
            }
        }
        *at = i;
        None
    }

    /// Fires every waiter of group `index`, in registration order, and unprimes it: the
    /// waiters that are this simulation's tasks are queued in `ready`, in order, when all
    /// of them are; otherwise every waker goes to `out`, for [`wake_sorted`] to wake in
    /// order.
    pub(crate) fn fire_group(&mut self, index: u32, ready: &ReadyQueue, out: &mut Vec<Wake>) {
        debug_assert!(self.walking, "invariant: a group fires inside a walk");
        let sim_gen = self.sim_gen;
        let g = &mut self.groups[index as usize];
        self.edge_live -= g.live;
        // Queueing a task is all its waker's `wake` would do, in the same order; a
        // foreign waker may run any code, so with one among them all are woken in order
        // later, outside the table's borrow.
        let all_own = !g
            .waiters
            .iter()
            .any(|w| matches!(w.wake, EdgeWake::Foreign(_)));
        // The buffer stays the group's, for its next priming.
        for w in g.waiters.drain(..) {
            match w.wake {
                EdgeWake::Gone => {}
                EdgeWake::Task(task, gen16) if all_own => ready.push(task, gen16),
                EdgeWake::Task(task, gen16) => {
                    out.push((w.seq, waker::task_waker(sim_gen, task, gen16)));
                }
                EdgeWake::Foreign(waker) => out.push((w.seq, waker)),
            }
        }
        self.unprime(index);
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
        self.waiters.len() + self.edge_live
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
        for g in &self.groups {
            let on = WaitOn::Edge {
                slot: g.slot,
                edge: g.edge,
            };
            for w in g.waiters.iter().filter(|w| w.task == Some(task)) {
                found.push((w.seq, self.describe_on(on, names, precision)));
            }
        }
        found.sort_by_key(|(seq, _)| *seq);
        found.into_iter().map(|(_, d)| d).collect()
    }

    /// Whether every unfired registration of `task` is on a wait list (a task, event, lock
    /// or queue), and there is at least one.
    pub(crate) fn only_lists(&self, task: Key) -> bool {
        let edge = |g: &Group| g.waiters.iter().any(|w| w.task == Some(task));
        if self.groups.iter().any(edge) {
            return false;
        }
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
pub(crate) fn wake_sorted(k: &Kernel, wakes: &mut Vec<Wake>) {
    if !wakes.is_sorted_by_key(|(seq, _)| *seq) {
        wakes.sort_by_key(|(seq, _)| *seq);
    }
    for (_, waker) in wakes.drain(..) {
        // A task of this simulation is queued here, which is all its waker's `wake` would
        // do (`state::wake_task` finds `k` current); any other waker is woken.
        match waker::own_task(&waker) {
            Some((sim_gen, index, gen16)) if sim_gen == k.sim_gen => {
                k.ready.push(index, gen16);
            }
            _ => waker.wake(),
        }
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
    wake_sorted(k, &mut wakes);
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
    /// Already removed from the table ([`take_fired`](Self::take_fired)): `Drop` has
    /// nothing left to do.
    released: bool,
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
        k.refuse_in_sync("awaiting a trigger");
        let waiter = NewWaiter {
            on,
            task: k.current.get(),
            waker: cx.waker().clone(),
            at,
        };
        let key = k.waits.borrow_mut().add(waiter);
        Self {
            sim_gen: k.sim_gen,
            key,
            released: false,
        }
    }

    /// As [`poll_fired`](Self::poll_fired), and once fired also removes the registration
    /// from the table, in the same visit to the kernel, so that dropping it next costs no
    /// lookup of the current kernel. For a trigger whose future is done once it fires.
    pub(crate) fn take_fired(&mut self, cx: &Context<'_>) -> bool {
        let key = self.key;
        let fired = state::with_sim(self.sim_gen, |k| {
            let mut waits = k.waits.borrow_mut();
            let Some(w) = waits.waiters.get_mut(key) else {
                return false;
            };
            if !w.fired {
                if !w.waker.will_wake(cx.waker()) {
                    w.waker.clone_from(cx.waker());
                }
                return false;
            }
            waits.remove(key);
            true
        })
        .unwrap_or(false);
        self.released |= fired;
        fired
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
        if self.released {
            return;
        }
        state::with_sim(self.sim_gen, |k| k.waits.borrow_mut().remove(self.key));
    }
}

/// Where an edge registration is: its group, the priming of the group it joined, and its
/// own `seq` (its place in the group).
#[derive(Clone, Copy, Debug)]
pub(crate) struct EdgeKey {
    group: u32,
    primed: u64,
    seq: u64,
}

/// An edge trigger's registration, in its group; dropping it deregisters.
#[derive(Debug)]
pub(crate) struct EdgeRegistration {
    sim_gen: u32,
    key: EdgeKey,
    /// Already fired and seen by [`take_fired`](Self::take_fired): `Drop` has nothing left
    /// to do.
    released: bool,
}

impl EdgeRegistration {
    /// Registers the current task as waiting on an edge of `slot`, to be woken through
    /// `cx`'s waker.
    pub(crate) fn new(k: &Kernel, slot: u32, edge: EdgeKind, cx: &Context<'_>) -> Self {
        k.refuse_in_sync("awaiting a trigger");
        let model = k.models.borrow();
        let key = k
            .waits
            .borrow_mut()
            .add_edge(slot, edge, k.current.get(), cx.waker(), &model);
        Self {
            sim_gen: k.sim_gen,
            key,
            released: false,
        }
    }

    /// Whether the registration has fired; if not, makes sure `cx`'s waker is the one
    /// woken. Once it has, `Drop` costs no lookup of the current kernel.
    pub(crate) fn take_fired(&mut self, cx: &Context<'_>) -> bool {
        let key = self.key;
        let fired = state::with_sim(self.sim_gen, |k| {
            let mut waits = k.waits.borrow_mut();
            if waits.edge_fired(key) {
                return true;
            }
            let sim_gen = waits.sim_gen;
            if let Some(w) = waits.edge_waiter(key)
                && !matches!(w.wake, EdgeWake::Gone)
                && !w.wake.wakes_as(cx.waker(), sim_gen)
            {
                w.wake = EdgeWake::of(cx.waker(), sim_gen);
            }
            false
        })
        .unwrap_or(false);
        self.released |= fired;
        fired
    }
}

impl Drop for EdgeRegistration {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        state::with_sim(self.sim_gen, |k| k.waits.borrow_mut().remove_edge(self.key));
    }
}
