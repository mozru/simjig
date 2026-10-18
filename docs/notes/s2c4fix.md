# Stage 2, review cycle 4 fixes (agent "s2c4fix", branch `fix/s2c4fix`)

Cycle 3 approximated cocotb's combinator timing with one or two yields after polling the
operands inline (`combinator::hop`). The cycle-4 review found two scenarios where that
diverges (`loser_scheduled`, `spawned_combinators`) and an O(G²) value-change walk. This
cycle replaces the approximation with cocotb's mechanism and fixes the walk.

## cocotb 2.1's mechanism (read from the pinned sources)
- `_concurrent_waiters._wait`: `waiters = [Task(a) for a in awaitables]`, then
  `for task in reversed(waiters): task._start_next()` (front of the queue, operand order).
  The caller awaits an `_InternalEvent` (`complete`), set by the done callback of the last
  helper to finish; `set` schedules the caller at the back.
- `FIRST_COMPLETED` (`First`, `select`, `with_timeout = select(Timer(t), x)`): the first
  helper to *finish* is `first_completed`, and the done callback cancels the rest
  (`cancel_remaining`, operand order). `FIRST_EXCEPTION` (`Combine`, `gather`): a helper
  that finishes cancelled or raising does the same.
- `Task.cancel()`: `PENDING` deregisters the trigger at once (`_trigger_callback.cancel()`)
  and schedules the task at the back with `CancelledError`; `SCHEDULED` keeps its queue
  place ("hijack"). A cancelled `_wait` cancels its own helpers and loops on
  `NullTrigger()` (one hop each) until they finished, then re-raises.
- `NullTrigger._prime` reacts at once: awaiting it is one hop (= simjig `yield_now`).

## The simjig model (`kernel::combinator`, `kernel::tasks`)
- A helper is a task-table entry (`Entry::Helper`): root task, owner context, state
  (`Running`, `Cancelling`, `Done`), the count of started combinators with helpers
  running inside its operand (`live_forks`), and whether its cancellation was passed on.
  It has a slot in the ready queue and a waker of its own; no future, name, handle or
  join list. `TaskTable::len`, `unfinished`, `keys` skip helpers, so stats, reports and
  `drop_all` see only tasks.
- The operands stay in the combinator future (they borrow from the caller, so they cannot
  be `'static` kernel tasks). Running a helper polls its root task with
  `Kernel::route = Some(helper)`; each `Fork` on the path polls only the operand whose
  helper is, or contains, the target (`tasks::route_index` walks owners), setting
  `Kernel::context` to that helper and giving the operand the helper's waker. Futures off
  the path are polled again with their own task's waker, a spurious poll they tolerate,
  so a foreign combinator never ends up holding a helper's waker.
- A `Fork` starts and returns only in a run of its owner context (`tasks::runs_owner`).
- Losers: a `Running` helper with no live nested forks has its operand dropped at once
  (cocotb's deregistration) and is queued (dedup keeps a scheduled helper's place); when
  it runs it finishes cancelled. One with live nested forks keeps its operand; its first
  run polls the operand once so the nested forks cancel their helpers, then it requeues
  itself until they finished.
- `with_timeout` is `Fork::first` over `[Timer, future]`.
- `__fork!` binds every operand with one pattern; each recursion step names its operand
  `op`, and macro hygiene makes the names distinct.

Why not simpler (rule 5): the cycle-3 yields cannot model "first to finish in run order"
(it picks the leftmost ready operand, `loser_scheduled`) or the queue place of a helper
woken by another task (`spawned_combinators`). Spawning real tasks would need `'static`
operands (an API break: every `first!(ev.wait(), ..)` borrows) or an `unsafe`
pointer to the pinned operand (a new `unsafe` module outside the three allowed). Routing
the root poll needs neither. Cost: one helper entry per operand (no allocation) and two to
three more polls of the root per combinator; see the benchmark.

Executor-semantics change (rule 4): winner choice and caller resumption follow the
helpers; trace hashes of runs with combinators change. CHANGELOG has the entry.

## Value-change walk and wait containers (`kernel::waits`)
- `groups: BTreeMap<u64, Group>` keyed by priming `seq`: prime, fire and unprime are
  O(log G), a walk step is a `range_mut` from where the last one stopped, so a walk over
  G groups is O(G log G) (was `Vec::remove` per fired group, O(G²)).
- Group waiters and timer deadlines are `Pending` (a `VecDeque<Key>` and a live count):
  a cancelled registration leaves the slab at once and its stale key is skipped, and
  compacted once more than half are stale. Cancelling one of W waiters and firing one of
  W timers due together are O(1) (were O(W)). An empty group or deadline is still removed
  at once, so no timestep is scheduled for a deadline whose timers were all cancelled
  (`cancel::cancelling_most_waiters_of_one_edge_or_deadline_leaves_the_rest_in_order`).
- The `until` bound is kept, with a comment: it mirrors `callValueCbs`; with writes in the
  write log a group primed during the walk could not fire anyway.
- The global `WaitTable::cancelled` counter and `cancelled_waits` are gone (nit N3).

## Benchmark (reviewer's `bench_s2c4.rs`, release, best of 5, 200 cycles, osx-arm64)
Measured back to back on a shared machine (other agents building). Noisy runs, with load
above 20, are discarded.

| Scenario | pre-cycle-3 `162805c` | stage/2 `191b106` | this branch |
|---|---|---|---|
| one_group N=1000 | 11.8 ms | 10.8 ms | 11.2 ms |
| many_groups N=1000 | 24.0 ms | 139.7 ms | 38.6 ms |
| cancel_heavy N=1000 | 66.1 ms | 92.2 ms | 108.7 ms |
| one_group N=4000 | 43.9 ms | 42.3 ms | 43.3 ms |
| many_groups N=4000 | 134.4 ms | 2747.6 ms | 155.2 ms |
| cancel_heavy N=4000 | 613.5 ms | 703.2 ms | 433.8 ms |

The reviewer measured 45 ms for pre-cycle-3 many_groups N=4000 on their run; on this
machine the same commit takes 134 ms, so the 2x budget is taken against 134 ms. The helper
overhead is what remains in cancel_heavy N=1000 (1.18x stage/2); the lazy containers
pay for it at N=4000.

## Goldens (`tests/conformance/tb/test_callback_order.py`, counter DUT)
The reviewer's eight scenarios, renamed (`r_` prefix dropped; `r_nested` →
`nested_combinators`, `r_timeout_first` → `timeout_combinators`, `r_many` →
`many_kinds`, `r_spawned_combos` → `spawned_combinators`), plus:
- `sync_losers` (director nit): `First` losers waiting on an `Event`, a `Lock` and a
  `Queue` (`select`, as `First` takes no coroutine);
- `cancelled_nest`: a `First` whose loser is a waiting `Combine`; the caller resumes
  between z3 and z4 of an Event chain, as predicted from the source before the run.

On the stage/2 kernel, `loser_scheduled`, `spawned_combinators` and `cancelled_nest` fail
and the other seven pass. All 29 goldens reproduce byte for byte, and all 29 replays
match. FakeModel tests: `phases::first_goes_to_the_helper_that_finishes_first_not_the_leftmost_operand`
and `phases::a_cancelled_nested_combinator_finishes_its_helpers_before_the_caller_resumes`
(both fail on stage/2).

## Experiments (not goldens; scripts in the scratchpad)
- A `First` loser on `Lock.acquire()` granted the lock in the deciding callback, then
  cancelled: cocotb leaves the lock held by the cancelled helper and a later waiter never
  gets it (`locked=True` 20 ns later). simjig passes it on. README "Known differences".
- Awaiting an already-set `Event`: cocotb resumes the waiter after the tasks already
  queued (one hop); simjig returns at once. By cocotb's source a free `Lock.acquire()`
  does the same. Not changed here (sync primitives were outside this brief); in the README
  as a known, not deliberate, difference.

## Edges on multi-bit signals
cocotb 2.1 rejects them: `LogicArrayObject.rising_edge` raises `TypeError` for
`len(self) != 1`. simjig already rejects them (`trigger::edge` panics; typed handles fail
at compile time). `fire_next_change` documents that it compares bit 0, the whole value
then. Pinned by `phases::edges_of_a_multi_bit_signal_are_rejected_as_in_cocotb`.

## Director's cycle-4 nits
`until` comment (above); sync losers golden; `kernel.md` and `duts.md` surprise 5 point to
the README; DECISION §0 "Within one value-change comparison"; ARCHITECTURE single home
for scheduler semantics; `phases` rename to `…in_priming_order…`; `wake_sorted` says a
panicking foreign waker escapes `Sim::run` (from the scheduler loop) or becomes the
task's `TaskPanic` (from inside a task).

## Not verified / open
- Linux.
- A nested combinator owned by a helper whose operand also polls foreign futures: a
  cancel-run polls that operand once (to reach the nested forks), which can make such a
  foreign future progress where cocotb would throw `CancelledError`. No golden or test.
- A nested `Sim::run` inside a combinator operand now finds the thread-local borrowed;
  `install` maps that to the same "cannot be nested" panic.
