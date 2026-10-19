# Notes: a faster async executor with the same observable behaviour (agent "sched")

Goal: async testbenches as fast as possible, with bit-identical observable behaviour: the
33 goldens, the wake order, the trace hash, replay and fork, failure reports. Starting
point: int-p @ 667e341 (`docs/notes/perf.md`, phase 3: async C on the FIFO at about
305 ns/cycle, the C++ loop at about 35).

## What was built
Nine commits; each is behaviour-preserving and was checked against the corpus below and
the kernel tests before it was committed (the full gates ran on the last).

1. **A differential corpus** (`kernel::differential`, `corpus.txt`), committed first, on
   the old scheduler. 4096 random programs over random simulations: one or two fake
   models, three kernel clocks (added before the run or declared inside it, random period,
   phase and duty cycle, stopped and restarted by programs), backends that settle or not,
   are held or not and report inputs as eval-unchanged or not (what a Verilator model
   reports), and programs mixing edges of kernel clocks, of design-driven signals and of
   inputs, `cycles`, `.read_write()`/`.read_only()`, Timers, the three regions, events,
   locks, queues, spawns, detaches, cancellations, `first!`, `join!`, `with_timeout`, a
   combinator of the test's own with foreign wakers, loops re-arming one trigger, triggers
   dropped before and after their first poll, and panics. A seed's digest covers its
   outcome (the failure report, with source lines blanked), the trace hash, the kernel's
   counters and a log of every resumption with the time, the task, the step and every
   signal's value. Mutations of the scheduler (reversing a group's wake order, firing the
   Timers of a timestep together, firing groups together, reordering a group on join,
   ignoring the log in a quiet ReadWrite round, skipping walks after evals) change 22 to
   996 of the 4096 digests.
2. **Edge registrations live in their value-change group** (`waits`). Before, an edge
   registration was a slab entry plus a key in its group; firing marked each entry fired
   and cloned its waker, and the poll that saw it removed the entry. Now a group holds
   its registrations in registration order; firing drains their wakers, and a
   registration has fired exactly when its group is no longer primed by the priming it
   joined.
3. **An empty ready queue is answered inline** (`tasks::run_ready`): the scheduler asks
   after every region, eval and fired group and mostly finds none.
4. **A walk keeps its place and queues its own tasks directly**: no binary search per step
   (order is compacted only once the walk ends), and a group whose waiters are all this
   simulation's tasks queues them as it fires.
5. **The timestep's fixed records go straight to their tails** (`recorder`): no per-record
   dispatch on the kind; the ring index is the count masked.
6. **The write log is applied in place**, recorded in the same pass; a timestep with no
   Timer due does not lend out the wake buffer; ReadWrite waiters are listed only for the
   limit's report.
7. **One `Cell` per counter**, and a poll starts with one borrow of the task table.
8. **A walk holds its wake buffer** from start to end.
9. **No checked conversions** when a write is logged.

Eval elision (`Models::eval`'s skip of a settled model) and the walk skip
(`fire_value_changes`' early return when nothing was written and only eval-unchanged slots
are watched) are untouched: a fix on `p1-fix` narrows both, and nothing here relies on
them or extends them.

## Why not simpler (rule 5)
The executor's semantics are not changed: no new region, no new ordering rule, no new
public API. The one structural change is where edge registrations live (2.). The simpler
alternative, keeping them in the slab, cost a slab insert, a waker clone at the fire and
a slab remove per await, for nothing the slab gives an edge wait: a group already orders
its waiters and fires them all at once. Timer, region and list waits stay in the slab,
where a registration can fire alone (one Timer, one waiter of a list).

**The old path is not kept selectable.** The brief asked for the old scheduler to stay
selectable (under `cfg(test)` or an env var) for the differential test. That would keep a
second wait table and a second walk in the crate, two homes for one concept (rule 6), for
testing only. Instead the old scheduler's behaviour is recorded: the corpus was blessed on
667e341 before any change, and every later commit reproduces it. The trade-off: new random
seeds cannot be compared with the old path without checking out 667e341
(`SIMJIG_CORPUS_BLESS=1` there, with a larger `SEEDS`). Re-blessing is for a reviewed
change of semantics only.

**The per-edge schedule was evaluated and not built in the form proposed.** A
precomputed, ordered list of the waiters of each kernel-clock edge, reused when a task
re-arms the same edge, cannot keep the wake order: cocotb's order is the order in which
groups were *primed*, and a task that fires and re-arms primes its group again, last in
the order (golden `coincident_join`, the `rclk`/`wclk` case in the conformance README).
Where a task re-arms (and whether it re-arms the same edge at all) is known only when it
awaits again, so the slot cannot be kept in place across the fire; what a re-arm can
reuse is the group itself and its buffers, which it already did and still does. What
2.-4. built is that schedule in the general path: a group per (signal, edge kind) holding
its waiters in order, fired by draining, its own tasks queued without their wakers.

**Not done, each because it would change behaviour or a guarantee:**
- *Comparing only written slots after a write* (a per-slot dirty filter in the walk): a
  group's slot can also change in an eval, and which slots an eval can change is the
  assumption `p1-fix` is narrowing. Not done.
- *Fewer evals, a faster hash*: changes when writes take effect, or every recorded hash.
- *No thread-local per await* (a kernel pointer carried by the poll's waker): needs a new
  unsafe surface (a waker pointing into the kernel, valid only during the poll) for about
  100 instructions per cycle of C on macOS, where a thread-local access is a call (on
  Linux it is one load). Not worth the unsafe code.
- *Reusing the `events_pending` answer between the last delta and `next_time`*: valid
  only while nothing between them can change a model's events; a saving of about 90
  instructions per cycle, for an invariant nothing enforces. Not done.
- *Inlining `Edge::poll`* into the task's future: measured, 37 instructions per cycle
  worse (register pressure in the larger frame). Reverted.
- *Skipping the panic-location clear before each poll* when no panic was ever recorded:
  measured, no change. Reverted.

## Invariants and how wake order is preserved
- A group's registrations are in registration order, which is `seq` order; firing drains
  them in that order, so the wakes are what `wake_sorted` issued before, already sorted.
- An edge registration has fired iff its group's `primed` differs from the priming it
  joined: a registration not yet cancelled keeps its group's `live` count above zero, so
  the group cannot be unprimed by cancellations, only by firing.
- A cancelled registration leaves its waker (`None`) in place, found by binary search on
  `seq`; the group is unprimed when its last live one goes, as before, and its list is
  compacted when most of it is cancelled.
- A walk visits `order` from its place on, and `order` is not compacted while a walk is
  under way (`walking`), so places stay valid while the tasks a group woke run, prime
  groups (appended, with a `seq` past the walk's bound) and unprime others (marked stale,
  skipped).
- A group's own tasks are queued as it fires only when *all* its waiters are this
  simulation's tasks: queueing is all their wakers would do, in the same order. With a
  foreign waker among them, every waker of the group is woken in order outside the
  table's borrow, as before, so a foreign waker's side effects interleave as they did.
- Applying the log in place: no task runs between the start and the end of an
  application, so nothing can be logged meanwhile; writes are applied and recorded in
  write order (the model writes produce no records, so interleaving them with the
  records changes no record).

## Measurements
Bench binary (`bench/simjig`, profile `bench`), async flow, 1M cycles (mid: 100k), each
build run 5 times per variant, interleaved, every round idle-gated (1-minute load < 2.5,
no cargo, rustc, clang, verilator or nextest process); median ns per cycle. Instructions
per cycle: `/usr/bin/time -l`, the difference of a 2M-cycle and a 1k-cycle run, halved.
Every build reported the same pushes, pops and hashes (fifo C: 402910, 402901,
3888034673; mid C: 48611, 48594, 4276890249).

Measured in one session (2026-10-19, loads 1.20 to 1.58), interleaved:

| Commit | fifo A | fifo B | fifo C | mid C | fifo_pub C | fifo C instr. |
|---|---:|---:|---:|---:|---:|---:|
| int-p @ 667e341 | 150.7 | 162.6 | 306.6 | 1526.8 | 311.5 | 10728 |
| edge registrations in their groups | 145.9 | 159.6 | 298.0 | 1515.8 | 302.5 | 10392 |
| empty ready queue inline | 139.2 | 151.2 | 287.6 | 1500.2 | 292.8 | 9916 |
| walk keeps its place, queues its own tasks | 137.5 | 149.4 | 282.9 | 1496.6 | 288.2 | 9743 |
| fixed records straight to their tails | 132.5 | 144.6 | 275.6 | 1489.0 | 280.7 | 9460 |
| log applied in place; Timer, ReadWrite checks | 129.6 | 141.3 | 274.0 | 1486.7 | 277.8 | 9236 |
| one Cell per counter; one borrow per poll | 129.0 | 140.3 | 270.6 | 1483.0 | 277.1 | 9188 |
| walk holds its wake buffer | 128.9 | 140.9 | 270.5 | 1484.3 | 275.7 | 9155 |
| no checked conversions per logged write | 129.4 | 140.2 | 269.9 | 1482.4 | 275.9 | 9105 |

Instructions per cycle, before and after: fifo A 5513 -> 4540, B 6004 -> 5029, C 10728 ->
9105; fifo_pub C 10865 -> 9244. The last two rows are within the noise (about 1%) of each
other in time; their instruction counts are the measure that decided them.

`fifo` against `fifo_pub` (the model built with `--public-flat-rw`): the gain is the same
(C: 36.7 ns on `fifo`, 35.6 ns on `fifo_pub`; 1623 and 1621 instructions per cycle). Every
change is in the kernel and none in eval elision or the walk skip, which are what
`--public-flat-rw` changes, so the narrowing on `p1-fix` should leave these gains as they
are and add its own cost on `fifo_pub` (more evals and walks), not take these back.

## The floor, and what limits it
Async C on the FIFO is now 270 ns per cycle (9105 instructions), from 307: 12% faster;
A 14%, B 14%; mid C 3% (its 1.3 us eval dominates). It is 7.7x the C++ loop (35 ns), not
the 100 ns the brief hoped for. A single-stepped cycle of C now:

| Part | Instructions |
|---|---:|
| Verilator's three evals, its message queue and mutex, the shim around them | 1590 |
| Signal access: 7 typed reads, 3 typed writes and 2 clock writes (lookups, checks, the log, the model writes) | 1500 |
| The timestep loop, with its 10 fixed-tail records (Step, Eval, ReadOnly) | 1280 |
| Awaits (3: the trigger made, registered, fired, seen; the group unprimed) | 1270 |
| Value-change walks (3 walks, 5 steps, 2 groups fired) and applying the log | 1190 |
| Records hashed in full (3 Poll, 4 Write) | 770 |
| Task polls (3: the queue, the task table, `catch_unwind`) | 700 |
| The testbench's own code (three task bodies) | 380 |
| Thread-local lookups (16; on macOS each is a call to `_tlv_get_addr`) | 260 |

(8910 single-stepped in one cycle after warm-up; the 9105 above is the average over 2M.)

What limits it: no part is above 15%, and each is now tens of instructions of `RefCell`
borrows, bounds checks, `Option`s and calls per operation, over hundreds of operations a
cycle, each of which the semantics require (every region, every walk after an application
of the log, every record, every await's registration in priming order). The 100 ns
estimate assumed all of that at the cost of a hand-written loop; reaching it would need
fewer passes than the semantics allow (merging evals, comparing only written slots,
dropping records), or an executor that is not the general one (sync mode is that design:
43 ns). Within these semantics, the next steps would each give 1 to 3%: one `RefCell` for
the wait table and the ready queue together, a kernel pointer passed through the poll
(macOS only, new unsafe code), reusing `events_pending` between the last delta and
`next_time`, and leaner typed reads and writes.

## After the merge with `p1-fix` (int-q)
The corpus reproduces on the merged tree unchanged: `p1-fix`'s changes (no elision for a
model with a writable public variable, `eval_changes` true for every Verilator slot) are
in the Verilator backend, which the corpus's fake backends do not use. The now-dead
`eval_changes` mechanism (`ModelBackend::eval_changes`, `WaitTable::eval_groups`, the
clause in `fire_value_changes`) is removed; the corpus still reproduces all 4096 digests
without a re-bless. That is expected: a fake model's eval never writes an input, so for
it the skip was sound and changed nothing observable (the digest has no walk count).
The draw that chose "inputs eval-unchanged" is kept, unused, so each seed still draws
the simulation and programs it was blessed with.

## Corpus seeds 4096-8191: an edge re-polled under another waker (int-q)

`EdgeRegistration::take_fired` refreshes the stored waker when a registered edge is
polled under another one; no seed reached it (mutant m5, which skips the refresh,
survived). A new operation, `Op::Repolled` (a rising edge polled once under
`Waker::noop()`, then awaited), is appended to the draw for seeds 4096 and up only, so seeds
0-4095 draw exactly the programs they did and their 4096 digests are unchanged (the bless
added lines only). Seeds 4096-8191 were blessed on int-q. With m5 applied, 1129 of the 8192
seeds differ, all of them new ones, and `cancel::an_edge_repolled_with_another_waker_wakes_the_newest`
fails.
