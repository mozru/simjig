# Stage 2, review cycle 5 fixes (agent "s2c5fix", branch `fix/s2c5fix`)

## 1. Waits that are ready when awaited

### cocotb 2.1, from the pinned sources (`_base_triggers.py`, `queue.py`, `task.py`)
- `Task._advance` registers the awaited trigger with `trigger._register(self._schedule_resume)`;
  `_register` primes it, and a trigger that fires while primed calls the callback at once:
  `_schedule_resume` puts the task at the **back** of the event loop (`schedule`, not
  `schedule_left`). So "ready when awaited" is one hop, behind every task already queued.
- `Event.wait()` returns the event's one `_Event` trigger; `_Event._prime` fires it when
  the event is set. The callback has fired by then, so a `clear()` before the task runs
  does not stop it.
- `Lock.acquire()` returns a fresh `_Lock`; `_prime` calls `Lock._prime_lock`, which takes a
  free lock (`_current_acquired = lock`) and fires. The lock is held during the hop.
  `release()` hands it to the oldest pending `_Lock` the same way.
- `Queue.get()` / `put()` are coroutines: `while self.empty(): await Event().wait()`, then
  `get_nowait()`. On a non-empty (non-full) queue they await nothing: **no hop**. A put
  wakes the oldest getter (`_wakeup_next`) by setting its private Event; the getter
  re-checks `empty()` when it runs, so a task that runs before it takes the item, and the
  woken getter appends a new Event at the back. `put_nowait` checks only `full()`.
- `await task` on a finished task returns at once (`Task.__await__` yields only if not
  done). simjig's join does the same; not changed.

The brief assumed a hop for Queue too; cocotb takes none there. What differed for Queue
was fairness: simjig queued a fresh `get` behind waiting getters. Both are now as cocotb.

### Experiments, then goldens (`tests/conformance/tb/test_sync_ready.py`, counter DUT)
`event_ready`, `lock_ready`, `queue_get_ready`, `queue_put_ready`: three rounds each (ready,
woken by another task, and interleaved with other tasks the same edge wakes; Event inside a
`First`). Every line matched the prediction from the source before the first run. All four
replays (`e2e_golden/test_sync_ready.rs`) failed on stage/2 and pass now; the 29 older
goldens still reproduce byte for byte, and their replays still match.

### simjig (`kernel::sync`, `kernel::trigger`)
- `EventWait`: `Fresh | Hop(YieldNow) | Waiting(Registration)`. A set event takes the hop.
- `LockFuture`: `Fresh | Hop(LockGuard, YieldNow) | Waiting(Registration)`. A free lock is
  taken before the hop (guard held in the future); dropping the future during the hop
  drops the guard, which passes the lock on (as for a granted waiter, below).
- `Queue`: `get`/`put` complete whenever they can; the woken flag and the
  `has_waiters` check are gone. `try_put` fails only when full.
- `YieldNow` ends only in a run of the context that awaits it (`tasks::runs_owner`). A
  hand-written join in a task's future is polled again when that task's combinator helpers
  run (routed polls); that poll used to end the hop early, before tasks queued ahead of it
  (`phases::a_hop_ends_in_its_own_run_not_in_a_combinator_helper_run_of_its_task`). The
  sync hops reuse `YieldNow`, so they inherit this.

Why not simpler (rule 5): the hop could be a fresh `wake_by_ref` + flag in each future,
but that is `yield_now` again (rule 6, one place per concept), and without the own-run
check it ends early in the routed polls above. Cost: one thread-local read per hop poll.

Executor-semantics change (rule 4): CHANGELOG has three entries (yield_now, Event/Lock,
Queue). Trace hashes of runs that await a set event or a free lock change.

FakeModel tests (fail on stage/2): `sync::event_wakes_all_waiters_in_wait_order_and_a_set_event_resumes_after_the_ready_tasks`,
`sync::a_free_lock_is_taken_at_once_and_its_task_resumes_after_the_ready_tasks`,
`sync::a_get_that_finds_an_item_takes_it_ahead_of_a_woken_getter`,
`sync::a_put_that_finds_room_fills_it_ahead_of_a_woken_putter`.

## 3. The other known differences, re-checked by experiment
Scripts: scratchpad `s2c5fix/exp/tb/test_exp.py` (counter DUT, run through `regen.run_suite`).
- **Drop cleanup vs `finally`**: still true. A `select(ev.wait(), loser())` loser's
  `finally` runs after a task woken by the same `ev.set()` after the decider; simjig's
  `Drop` runs before it. Pinned: `cancel::a_cancelled_losers_drop_cleanup_runs_when_it_is_cancelled`.
- **Lock granted to a cancelled loser**: still true, and now also reachable with a free
  lock: `First(set_ev.wait(), free_lock.acquire())` → the Event wins, `locked()` stays True
  and a later `with_timeout(lock.acquire(), 20 ns)` times out. Pinned:
  `sync::a_lock_taken_by_a_cancelled_first_loser_is_released`.
- **New, same family**: a `select(ev.wait(), q.get())` loser woken by a put and cancelled
  before it runs loses the wake in cocotb: the next getter never gets the item (`qsize()`
  stays 1). simjig passes the wake on. Kept as deliberate (a lost item is a hang, not a
  feature anyone relies on); merged into the README entry. Pinned:
  `sync::a_queue_wake_of_a_cancelled_first_loser_goes_to_the_next_getter`.

## 2. Flaky `fst_tracing_links_and_writes_a_trace`: root cause not established
What the record shows (s2c4fix's CI run, 2026-10-17 22:17-22:18 local):
- The failure was **not** FST-specific and **not** a timeout: the test failed after 36 s
  (nextest kills at 120 s) with `failed to run custom build command for
  simjig-fixture-vtable`. The five `fixture_crate` tests take turns on one fixture crate
  (a file lock); the first one does the cold build for all, and that was the FST test.
  The four after it reran the build script and passed. The build script's stderr was in
  the assertion message but the agent's `grep` dropped it, and the fixture's
  `target/debug/build/*/stderr` was overwritten by the passing rerun.
- The fixture cache (`target/tmp/fixture-vtable/target/simjig-cache`) shows a cold build:
  every entry was created between 22:17:55 and 22:18:18, all five models and both
  runtime archives, so the failing run got past every Verilator run and cached C++
  compile; it failed in the last ~7 s (model compiles into `OUT_DIR`, linking
  directives). No `tmp/` work directory was left.

Tried, all passed: 7 full `cargo nextest run --workspace --all-features --profile ci`
runs with the fixture target deleted first (cold), at load averages 6 to 94 on 12 cores,
3 of them with `SIMJIG_CCACHE=0` (cold C++ compiles, as on a fresh worktree). Ruled out by
reading: the `.fst` path (per-pid, under `$TMPDIR`), the FST writer (no temp files),
svtypes' JSON-pass temp dirs (atomic `create_dir`), cache work dirs (pid + counter +
time, published by `rename`), the cross-process lock. Not ruled out: an environmental
failure during a cold C++ compile (e.g. fork limits: `kern.maxprocperuid` is 2666 and
about ten agents were building at once); without the stderr this cannot be told apart.

No code change for this item: I did not find a cause, and a retry or a longer timeout
would hide one. Next time, keep the full nextest output (the message includes the build
script's stderr).

### A deterministic bug found on the way (in `simjig-build/src`, not owned here)
`int-s3`'s ci7.log fails all five fixture tests with `model prefix Vcounter_8f7c9dab is
already used by a different configuration`. Cause: `pipeline::identity` hashes
`format!("{config:?}")`, while `DutConfig::prefix` hashes the fields. A simjig-build
change that alters `DutConfig`'s `Debug` but not the prefix (668a3cc added
`generated: Vec<_>`, empty for these DUTs, so the prefix stayed and the identity changed)
leaves a `prefixes/<prefix>` claim from the old version in any persistent cache, and every
later build fails with "rename the DUT". The fixture keeps its cache under a stable path,
and so does every user crate (`target/simjig-cache`). Suggested fix: version the claim
with the simjig-build version (or derive the identity from the same fields as the prefix,
with a schema tag), so an upgrade starts a fresh claim.
