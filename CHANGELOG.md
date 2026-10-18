# Changelog

All notable changes to simjig. Constitution rule 4 requires an entry for every change to
the public API, the generated code or simulation semantics.

## Unreleased
- Initial workspace:
  - `svtypes`: the IR from Verilator `--json-only`;
  - `simjig-bits`: bit-vector values;
  - `simjig-build`: the pipeline, codegen and C ABI runtime;
  - `simjig`: kernel, model, typed handles, harness;
  - `simjig-macros`: `#[simjig::test]`;
  - `cargo-simjig`: `doctor`.
- C ABI version 1, vtable size 192, with trace, coverage and RNG slots.
- IR schema 2: adds `unrepresentable` constants.
- Scheduler: `Timer`s due at the same time fire one at a time, in registration order,
  each followed by every task it makes ready (and every task those wake), as cocotb runs
  its event loop after each Timer callback. Before, all of them woke together and tasks
  they woke ran after the last Timer task.
- Scheduler: value changes fire one (signal, edge kind) group at a time, oldest-primed
  first, each followed by every task it makes ready, as cocotb runs one VPI value-change
  callback at a time with its event loop inside each (README "Callback order"; goldens
  `coincident_join`, `changed_join`, `written_join`, `edge_kinds`). A task woken by one
  change that awaits a signal whose group another task already waits on, and which has
  not fired yet, now resumes in the same delta (it used to wait for the next change), and
  tasks those wakes make ready run before the next group's waiters. `rising`, `falling`
  and `changed` on one signal are separate groups. Wake order at one delta changes from
  global registration order to priming order; trace hashes of runs where that differs
  change.
- `first!`, `join!` and `with_timeout` run each operand in an internal helper task, as
  cocotb 2.1's `First`, `Combine` and `with_timeout` do (`_wait`): the helpers start at the
  front of the ready queue in operand order, the first helper to *finish* decides a
  `first!` (not the leftmost operand that is ready: an operand woken by a task the same
  edge woke loses to one the edge woke itself), losers are cancelled as cocotb cancels
  them (dropped when deregistered, finishing at their queue place), and the caller
  resumes when the last helper finishes (goldens `combinator_hop`, `loser_scheduled`,
  `spawned_combinators`, `cancelled_nest` and seven more in `test_callback_order`).
  Before, operands were polled inline in the caller, leftmost first, and the caller
  yielded once or twice afterwards. The cost is one helper entry and two or three extra
  polls of the caller's task per combinator. Trace hashes of runs with combinators
  change. `__private::{hop, boxed}` are gone (macro internals).
- Performance trade-off (correctness over speed, constitution priority order): each wait on
  a set `Event` or a free `Lock` now costs one extra scheduler round (one poll), as cocotb
  requires. On an Event/Lock-heavy benchmark (N tasks, each cycle: edge, set-event wait,
  lock, queue op) this is about 2.4x slower (N=1000: 14–18 ms → 33 ms; N=4000: 56 → 141 ms);
  the cost per poll fell from 70 to 55 ns. Other scheduler benchmarks are within ±10%.
- `Event::wait` on a set event and `Lock::lock` on a free lock resume after the tasks
  already ready (one hop), as cocotb 2.1's `Event.wait()` and `Lock.acquire()` do (goldens
  `event_ready`, `lock_ready`); they used to continue at once. The free lock is taken
  before the hop, so a task that runs in between sees it locked. Trace hashes of runs
  that await a set event or a free lock change.
- `Queue::get` and `Queue::put` that can complete do so at once even when other getters
  (putters) wait or were woken and have not run yet, as cocotb 2.1's `Queue` does (goldens
  `queue_get_ready`, `queue_put_ready`); the woken one finds the item (slot) gone and
  waits again, last in line. They used to queue behind every waiter. `Queue::try_put`
  likewise fails only when the queue is full (cocotb's `put_nowait`), no longer when
  puts are waiting.
- `yield_now()` ends only in a run of the task (or combinator helper) that awaits it. A
  hand-written join in a task's future that also awaits `first!`/`join!` used to see it
  end early, when the combinator's helpers ran.
- Performance: value-change groups are kept in a map keyed by priming order, so a walk
  over G groups costs O(G log G) instead of O(G^2); cancelling one of many waiters of an
  edge or a timer deadline, and firing one of many timers due together, is O(1) instead
  of O(waiters).
- `with_timeout` breaks ties as cocotb 2.1 does (golden `timeout_ties`): the timeout's
  Timer registers before the future's triggers and is checked first, so a `Timer`,
  edge or `cycles` due exactly at the timeout now times out (a future's own `Timer` used
  to win); a spawned task whose `Timer` was registered before the call still returns its
  result.
- `BlockedTask` implements `Display` (`` `main.d` awaits rising(clk) ``), the rendering
  `Failure`'s `Display` and the failure report share.
- `#[simjig::test]` has a default simulation-time budget of 10 ms (`DEFAULT_TIMEOUT_MS`),
  rounded up to a whole tick (`ceil(10 ms / tick)`, at least one) at precisions coarser
  than 1 ms; `timeout = "none"` disables it. Before, a test without `timeout` had no
  limit.
- `TestCtx::rng(path)` panics, at the second call, when `path` was already taken in the
  run (the message names the first call's location): two streams with one path would
  silently repeat each other's values.
- `FailureKind::TimeLimit` gains `blocked`: the tasks still waiting, and on what, as for
  `Starved` and `Deadlock`.
- New `FailureKind::ReadWriteLimit { limit, waiters }`: more than `READ_WRITE_MAX`
  (10 000) `ReadWrite` rounds that woke a task in one timestep.
- New `FailureKind::DpiPanic { message }`: a DPI-C import implemented in Rust panicked
  during an eval; the model is poisoned.
- Times print in the largest unit that shows them exactly (`1250 ns`, `1 us`; zero in
  the precision's unit, `0 ps`), through `SimTime::display` (`TimeDisplay`), in failures,
  reports and `summary.json`.
- The repro line is `SIMJIG_SEED=<seed> [<launcher> ]cargo nextest run -p <package>
  [--run-ignored all] -E 'binary(=<binary>) & test(=<test>)'`, with the launcher
  (`pixi run [--manifest-path ..] [-e <env>]`, `conda run -p <prefix>`) the test ran under.
- Run directories are `<root>/<package>/<binary>/<test>/<seed>/` (was
  `<root>/<test>/<seed>/`), written to a temporary sibling and renamed into place.
