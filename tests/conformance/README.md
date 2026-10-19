# Scheduler conformance goldens

simjig's scheduler must reproduce what cocotb 2.x shows a testbench on Verilator. This
directory holds that reference behaviour as logs ("goldens") recorded from the pinned
toolchain, plus the cocotb tests that produce them. The Rust side runs the same scenarios
against the same DUTs, emits the same log format, and diffs against `golden/`.

- Toolchain: cocotb **2.1.0** (PyPI) and Verilator **5.052** (conda-forge), both pinned
  in the `conformance` pixi environment. `regen.py` refuses to run with any other cocotb,
  or with a `verilator` that is not the one in the pixi env.
- Build: cocotb's Verilator runner (`--vpi --public-flat-rw`, `--timescale 1ns/1ps`,
  `-Wall`), so one simulator step is 1 ps.
- `COCOTB_TRUST_INERTIAL_WRITES=1`, pinned by `regen.py`. This is cocotb's own default
  for Verilator (both its runner and its Makefile set it), so it is what cocotb users get.
  It also selects cocotb's C++ (GPI) clock driver.
- Each scenario runs in its **own simulator process**, so every log starts at time 0
  from a freshly constructed model.

## Commands
```
pixi run -e conformance conformance-regen           # rewrite golden/*.jsonl
pixi run -e conformance conformance-regen --check   # regenerate under target/ and require byte equality
python tests/conformance/show.py golden/timer.jsonl # print a log as a table
```
`--inertial 0 --out DIR` reruns everything with `COCOTB_TRUST_INERTIAL_WRITES=0` into
`DIR`, for comparison only (see "Write semantics").

## Layout
| Path | Contents |
|---|---|
| `regen.py` | the suite list (DUT, parameters, test module, scenarios), toolchain checks, `--check` |
| `tb/conflog.py` | the logger every scenario uses |
| `tb/test_<dut>.py` | the scenarios, one `@cocotb.test` each |
| `golden/<scenario>.jsonl` | one log per scenario |

| Scenario | DUT | What it pins down |
|---|---|---|
| `edge_read` | flop | values right after `RisingEdge` / `FallingEdge`; values at time 0 |
| `readonly_read` | flop | values in `ReadOnly` after a `RisingEdge` |
| `write_after_edge` | flop | when a write after `RisingEdge` becomes visible, and which edge captures it; last write wins |
| `timeout` | flop | `with_timeout`: trigger loses, wins, and ties with the timeout |
| `timeout_ties` | flop | `with_timeout` ties: a `Timer`, a sub-coroutine, a task started just before, an edge, `ClockCycles`; one tick short of a tie; a task whose `Timer` was armed first |
| `timer` | counter | `Timer` wake-ups between edges and exactly on edges; `Timer(1, "step")` |
| `clock_cycles` | counter | `ClockCycles`, rising and falling |
| `value_change` | counter | `ValueChange` on a DUT output and on a testbench-written input |
| `first_combine` | counter | `First` (who wins, ties) and `Combine` |
| `comb_settle` | comb_chain | combinational settle after a write: immediately, `ReadWrite`, `ReadOnly`, `ValueChange`, next `Timer`; a write issued in `ReadWrite` |
| `fifo_stream` | fifo | valid/ready stream with back-pressure; three concurrent tasks |
| `two_clocks` | async_fifo | two clocks with coincident rising edges: wake order and values; `Combine` of both edges |
| `cdc_stream` | async_fifo | data crossing the CDC end to end |
| `derived_edge` | derived_clk | `RisingEdge` on an internal derived clock next to `RisingEdge` on its source |
| `coincident_join` | async_fifo | callback order at coincident clock edges: a task woken on one clock that awaits the other joins its callback, or waits for the next edge |
| `changed_join` | comb_chain | the same for two outputs that change in one eval (`ValueChange(y)`, `ValueChange(y_eq)`) |
| `written_join` | comb_chain | the same for two inputs written in one delta |
| `edge_kinds` | counter | `ValueChange(clk)`, `RisingEdge(clk)` and `ClockCycles` on one signal are separate callbacks |
| `combinator_hop` | counter | `First`, `Combine` and `with_timeout` resume their caller after the tasks the same callback woke |
| `loser_scheduled` | counter | a `First` operand woken by a task the edge woke loses to one the edge woke, even when it is the left operand |
| `nested_combinators` | counter | `First` of a `Combine`, `Combine` of a `First`, `First` of a `First` |
| `timeout_combinators` | counter | `with_timeout` around a `First` and around a `Combine` |
| `cycles_join` | counter | `ClockCycles` awaited from a `ValueChange(clk)` wake joins the `RisingEdge(clk)` callback |
| `rise_changed` | counter | one task alternating `RisingEdge`, `ValueChange` and `FallingEdge` of `clk` among others looping on one kind |
| `many_kinds` | counter | 24 tasks cycling through the three edge kinds of `clk`, with an `Event` set in between |
| `primed_walk` | counter | callbacks registered during a walk (`ValueChange(count)` from a `clk` wake) and joined later |
| `spawned_combinators` | counter | `First`, `Combine`, `with_timeout` and a plain wait in spawned tasks, decided by an `Event` set in the edge callback |
| `sync_losers` | counter | `First` losers waiting on an `Event`, a `Lock` and a `Queue`: cancelled while waiting, they take nothing from later waiters |
| `cancelled_nest` | counter | a `First` whose losing operand is a waiting `Combine`: the caller resumes three hops after the edge |
| `event_ready` | counter | `Event.wait()` on a set Event resumes one hop later, after the tasks already queued, even if the Event is cleared meanwhile; on a cleared one, after the tasks woken before `set`; inside a `First` |
| `lock_ready` | counter | `Lock.acquire()` on a free Lock takes it at once and resumes one hop later; a released Lock goes to the oldest waiter, which resumes after the tasks already queued |
| `queue_get_ready` | counter | `Queue.get()` on a non-empty queue takes no hop; a task that runs before a woken getter takes its item, and the getter waits again behind the others |
| `queue_put_ready` | counter | the same for `Queue.put()` on a non-full queue and a woken putter |

## Log format
JSON Lines: one compact JSON object per line (`json.dumps(..., separators=(",", ":"))`),
keys in exactly this order, each line ending in `\n`:

```
{"time":15000,"phase":"active","trigger":"RisingEdge(clk)","task":"main","op":"read","signal":"q","value":0}
```

| Key | Type | Meaning |
|---|---|---|
| `time` | integer | simulation time in **picoseconds** when the line was recorded |
| `phase` | string | where the recording task is: `start` (the test body before its first `await`), `active` (woken by any trigger other than `ReadWrite`/`ReadOnly`), `readwrite`, `readonly` |
| `trigger` | string | label of the trigger that last resumed this task, or `start` |
| `task` | string | the scenario's name for the recording task (`main`, `driver`, `wclk_task`, ...) |
| `op` | string | `read` (value seen), `write` (value deposited), or `note` (an outcome that is not a signal) |
| `signal` | string | the signal's name relative to the top (internal nets by their plain name), or for a `note` the outcome's name (`outcome`, `first`) |
| `value` | integer or string | unsigned integer value for `read`/`write`; a string for `note` |

A `write` line is recorded at the moment the testbench issues the write, not when it takes
effect. Lines appear in the order the testbench executed them, across all tasks, so the
interleaving of tasks is part of the golden.

Trigger labels are fixed strings chosen by the scenario, not cocotb's `repr`:
`RisingEdge(sig)`, `FallingEdge(sig)`, `ValueChange(sig)`, `ReadWrite`, `ReadOnly`,
`Timer(<n><unit>)`, `ClockCycles(sig,n)`, `ClockCycles(sig,n,falling)`,
`First(a,b)`, `Combine(a,b)`, `with_timeout(<trigger>,<n><unit>)`, `Event.wait()`.

Every clock in these scenarios is a cocotb `Clock(sig, period, "ns")` started with
`start_high=False`: the signal is low for the first half period, so a 10 ns clock rises at
5, 15, 25, ... ns and falls at 10, 20, 30, ... ns.

## Observed semantics (cocotb 2.1.0 on Verilator 5.052, trusted inertial writes)
Line references are to the goldens.

### The time-step loop
cocotb's Verilator main loop (`cocotb/share/lib/verilator/verilator.cpp`), per time step:

1. Advance time. Run `NextTimeStep` callbacks, then timed callbacks: `Timer` wake-ups and
   the clock driver's toggles.
2. Repeat: `eval_step()`; apply the queued testbench writes; fire value-change callbacks
   (edges and `ValueChange`) for every watched signal that changed, whether the testbench
   wrote it or the eval changed it; repeat while writes are pending.
3. Fire `ReadWrite`; apply any writes made there; go back to 2 if anything was written.
4. `eval_end_step()`, then `ReadOnly`.
5. Next time = min(next cocotb timed callback, `nextTimeSlot()` if the design has
   `--timing` events). The simulation ends when neither exists.

The rules below follow from this loop.

### Edges on testbench-driven clocks see pre-edge state
- **After `RisingEdge(clk)`, flops still show their old values.** `edge_read` at 15 ns:
  `d=1`, `q=0`. `q=1` is visible only from `ReadOnly` at 15 ns (`readonly_read`) or at
  the next falling edge (`edge_read`, 20 ns). The clock's new value is applied as a write,
  and its edge trigger fires before the eval that clocks the flops.
- The same holds for every trigger built on a clock edge: `ClockCycles` (`clock_cycles`:
  `count=2` at the 35 ns edge, where the post-edge value is 3), `First`/`Combine`
  components, and `with_timeout(RisingEdge(..))`.
- `FallingEdge(clk)` sees the results of the previous rising edge (they settled in that
  time step).

### Edges on design-driven signals see post-eval state
- `ValueChange(count)` fires after the eval that changed `count`, so it sees the new
  value (`value_change`: `count=1` at 15 ns).
- `RisingEdge(div_clk)` on a derived clock (`derived_edge`) sees `div_clk=1` **and** the
  updated flops of both domains (`fast_count` and `slow_count` already incremented),
  while `RisingEdge(clk)` in the same time step sees the old values. Every source-clock
  edge task wakes before the derived-edge task.

### Timers wake before the time step's edges
- A `Timer` that expires at the same time as a clock edge resumes **before** the edge is
  applied: it reads `clk=0` and the pre-edge counter (`timer` at 15, 25, 35, 45 ns).
  `ReadOnly` in that time step then shows `clk=1` and the post-edge value.
- `First(RisingEdge(clk), Timer(t))` with both due at the same time: **the Timer wins**
  (`first_combine` at 25 ns).
- `with_timeout(RisingEdge(clk), t)` with the timeout at the same time as the edge:
  **`SimTimeoutError`** (`timeout` at 25 ns, reading `clk=0`). The same for
  `ClockCycles` (`timeout_ties` at 45 ns).
- `with_timeout(x, t)` is `select(Timer(t), x)`, which arms the timeout's Timer
  **before** `x`. Timers due at the same time fire one at a time, in the order they were
  armed, and cocotb runs every task a callback makes ready (and every task those wake)
  before the next callback. So a Timer due exactly at the timeout loses to it, whether
  awaited directly, inside a sub-coroutine, or in a task started just before
  (`timeout_ties` at 15, 20 and 25 ns: `SimTimeoutError`). A task whose own Timer was
  armed *before* the `with_timeout` call finishes first and its result wins
  (`timeout_ties` at 54.999 ns: `fired`).
- A trigger armed from a Timer wake-up at time t still catches the clock edge at t,
  because that edge has not been applied yet. In `first_combine`, `Combine(ClockCycles(clk,2),
  Timer(13ns))` armed at 25 ns counts the 25 ns edge as its first cycle, finishes its
  cycles at 35 ns, and resumes at 38 ns when the Timer expires.
- `Timer(1, "step")` advances exactly 1 ps.

### Write semantics
- A write is a deposit (VPI `vpiInertialDelay`). It is queued and applied right after the
  next `eval_step()` of the current time step, never immediately.
- **Reading a signal right after writing it, before awaiting, returns the old value**
  (`write_after_edge` at 15 ns; `comb_settle` at 1, 2, 3 ns).
- **A write right after `RisingEdge(clk)` is not captured by that edge.** The eval that
  clocks the flops runs before the write is applied. `write_after_edge`: `d` is written to
  1 at 15 ns, `q` stays 0 through `ReadOnly` at 15 ns and the read at the 25 ns edge (pre-edge
  state), and `q=1` from `ReadOnly` at 25 ns.
- The written value is visible from `ReadWrite` in the same time step, and so is its
  combinational fan-out: `comb_settle` shows the new `y` in `ReadWrite`, because an eval
  ran after the write was applied. `ReadOnly` and the next `Timer` agree.
- Two writes to one signal in the same delta: the last one wins (`write_after_edge` at
  25 ns, `0` then `1`, so `d=1`).
- A write to a testbench-driven input fires `ValueChange` on it in the same time step
  (`value_change` at 40 and 60 ns).
- A write issued in `ReadWrite` is visible in the following `ReadOnly`, with its fan-out
  settled (`comb_settle` at 4 ns).
- `ValueChange(y)` on a combinational output wakes before a `ReadWrite` waiter in the same
  time step (`comb_settle`, task `y_watch` before `main`).

With `COCOTB_TRUST_INERTIAL_WRITES=0` (not cocotb's Verilator default), only two goldens
change, both about `ReadWrite`: the `ReadWrite` waiter sees the **old** value of a signal
written earlier in that time step (`write_after_edge` at 15 ns reads `d=0`). In
`comb_settle`, `main` resumes in `ReadWrite` before `y_watch`, with the old `a` and `y`.
Every edge, Timer and ReadOnly observation is identical in the two modes.

### Coincident edges of two clocks
- Both clocks' new values are applied together, before either edge trigger fires. Each
  task woken at a coincident edge sees **both** clocks high and both domains' flops
  pre-edge (`two_clocks` at 75 and 105 ns).
- Wake order at a coincident edge is **not** clock-declaration order: the clock whose
  trigger was primed first (oldest) wakes its tasks first. In `two_clocks` the `wclk`
  clock was started first, yet the `rclk` waiters (including the `Combine` that completes
  on `rclk`) ran before the `wclk` waiter at 75 and 105 ns: the `rclk` trigger was primed
  at 60 ns, the `wclk` trigger re-primed at 65 ns after firing. See "Callback order".

### Callback order
cocotb 2.1 keeps **one trigger per (signal, edge kind)**: `RisingEdge(s)`,
`FallingEdge(s)` and `ValueChange(s)` are three triggers, each backed by its own VPI
value-change callback, registered when the trigger's first waiter arrives ("primed") and
removed when it fires or its last waiter leaves. Verilator's `callValueCbs`
(`verilated_vpi.cpp`) walks the callbacks in registration order, skipping those added
during the walk, and each callback compares the signal with its own copy of the value,
taken when it was registered. cocotb's `GPITrigger._react` runs the whole event loop
inside each callback. Hence, for one comparison (after an eval and the write
application that follows it):
- **Triggers fire one at a time, oldest-primed first**, and every task one wakes (and
  every task those wake, such as an `Event` waiter) runs before the next trigger fires
  (`coincident_join`, `changed_join`, `written_join`, `edge_kinds` at 15 ns: the
  `Event.wait()` task runs between the two triggers' waiters). Within a trigger, waiters
  resume in the order they started waiting.
- A task woken by one trigger that then awaits a trigger that **another task already
  waits on**, and which has not fired yet in this walk, **joins it** and resumes in the
  same walk (`coincident_join` 15 ns, `changed_join` and `written_join` 2 ns,
  `edge_kinds` 15 ns).
- If **nobody else waits** on that trigger, the task primes it afresh: its callback
  snapshots the value that already changed and waits for the next change
  (`coincident_join` 55 ns, `changed_join` and `written_join` 6 ns, `edge_kinds` 35 ns,
  where `RisingEdge(clk)` is primed from a `ValueChange(clk)` wake at a rising edge). The
  same holds for a trigger that **already fired** in this walk: its waiters re-prime it,
  last in the order (`coincident_join` 85 ns).
- `ClockCycles(s, n)` awaits the one `RisingEdge(s)` trigger `n` times, inline in the
  awaiting task, so it joins like a plain `RisingEdge` (`edge_kinds` 45 ns).

- **`First`, `Combine` and `with_timeout` run each operand in a helper task**
  (`_wait` in `_concurrent_waiters.py`; `with_timeout(x, t)` is `select(Timer(t), x)`).
  The helpers are started with `_start_next`, at the front of the queue in operand order,
  so they register before tasks started earlier but not yet run. The caller awaits an
  internal event set when the **last** helper finishes, which schedules it at the back.
  `First` and `select` are decided by the helper that **finishes first in run order**,
  not by the leftmost operand: an operand woken by a task that the same callback woke
  earlier (an `Event` it sets) loses to an operand the callback woke directly
  (`loser_scheduled`, both rounds). The decider cancels the other helpers (`Task.cancel()`):
  one waiting on a trigger is deregistered at once and scheduled at the back; one already
  scheduled finishes cancelled at its place. So the caller resumes after the tasks the
  callback woke directly (`combinator_hop` 25 and 35 ns) and, when a loser was still
  waiting, after the tasks those wake before the loser's cancellation runs (45 and 65 ns;
  `Combine` has no loser, 55 ns). A cancelled helper whose operand is itself a waiting
  combinator cancels that combinator's helpers when it runs and reschedules itself
  (`NullTrigger`) until they have finished: one more hop each (`cancelled_nest`).
  Combinators in spawned tasks follow the same rules (`spawned_combinators`).

simjig runs combinator operands the same way: each operand of `first!`, `join!` and
`with_timeout` is an internal helper task (`kernel::combinator`), queued, decided and
cancelled as above. Helpers have no name and no handle, and never appear in reports.
simjig mirrors this with one wait group per (signal, edge kind), ordered by the
registration that primed it (`kernel::waits`, "Value-change groups"), and a walk that runs
tasks to quiescence after each group that fires (`kernel::sched::fire_value_changes`).

### Event, Lock and Queue
- **`Event.wait()` on a set Event and `Lock.acquire()` on a free Lock take one hop.**
  `_Event._prime` and `Lock._prime_lock` fire the trigger while it is primed, which
  schedules the task at the back: it resumes after the tasks already queued (`event_ready`
  15 ns, `lock_ready` 15 ns), and still resumes if the Event is cleared meanwhile. The
  Lock is taken at that moment, so a task that runs in between sees `locked()` true.
- A set or a release schedules the waiter at the back too, after the tasks queued before
  it (`event_ready` 25 ns, `lock_ready` 17 and 25 ns). Inside a `First`, the hop is the
  helper's (`event_ready` 35 ns).
- **`Queue.get()` on a non-empty queue and `Queue.put()` on a non-full one take no hop**:
  they await nothing (`queue_get_ready` and `queue_put_ready` 15 ns).
- **A woken getter (putter) can lose its item (slot)** to a task that runs before it:
  `put_nowait` wakes the oldest getter, which loops on `while self.empty()`, while a task
  scheduled earlier calls `get()` and takes the item without waiting; the woken getter
  waits again, last in line (`queue_get_ready` 25 and 27 ns; `queue_put_ready` 25 to 31 ns).

simjig matches all of these (`kernel::sync`).

### Further rules, confirmed but not in a golden
Each is pinned by a simjig test against `FakeModel` (`crates/simjig/tests/kernel_tests`).
- `NextTimeStep` waiters resume before that time step's `Timer` waiters (step 1 above;
  `phases::next_time_step_resumes_at_the_next_scheduled_time_before_timers`).
- `First` of edges that fire in the same comparison goes to the operand whose edge
  trigger was primed first: with fresh triggers that is the leftmost, since the helpers
  register in operand order
  (`phases::first_of_coincident_edges_goes_to_the_trigger_primed_first`).
- A bus that changes and changes back within one time step, in two deltas, fires
  `ValueChange` **twice** (`phases::a_value_that_changes_and_changes_back_in_one_timestep_fires_changed_twice`).
- A task woken from a `ReadOnly` waiter (an `Event` set, a `Queue` put) runs in that
  `ReadOnly` phase. cocotb raises `RuntimeError` on a write there; simjig fails the run
  with `WriteInReadOnly`
  (`failures::a_task_woken_from_read_only_runs_there_and_cannot_write`).
- Under `--timing`, `Timer`s due at time t resume before the design's `#` events at t:
  they run in step 1, before the `eval_step()` that executes those events
  (`timing::a_timer_resumes_before_the_designs_event_at_the_same_time`).

### Known differences from cocotb
Each with the reason; all are deliberate:
- **A `Task` passed by value to `with_timeout`/`first!`/`join!` is cancelled** when the
  combinator drops it (a timeout, or losing a `first!`), because dropping a `Task` cancels
  it. cocotb keeps a Task operand running ("the callee continues to run"). To keep it
  running, pass `&mut task`, which matches cocotb byte for byte (scenario `r5_task_operand`; `cancel::a_task_operand_by_value_is_cancelled_and_by_reference_keeps_running`).
- **A timer of zero ticks** is rejected when it is first **awaited** (a `TaskPanic` naming
  the line that made it); cocotb raises `ValueError` when `Timer(0)` is **built**.
  `timer` is a `const fn`, which can neither report a run failure nor put the call
  site in the message, and a timer may be made before the run starts
  (`phases::trigger_misuse_points_at_the_line_that_made_the_trigger`).
- **Cancellation cleanup runs when the operand is cancelled.** simjig cancels by dropping
  the operand's future, at the moment cocotb deregisters a waiting helper; its `Drop`
  code runs then. cocotb throws `CancelledError` into the helper when it next runs, so its
  `finally` blocks run one hop later, after the tasks queued before it (seen in an
  experiment). The order of the helpers and the caller is the same (`kernel::combinator`;
  `cancel::a_cancelled_losers_drop_cleanup_runs_when_it_is_cancelled`).
- **A `Lock` or `Queue` wake granted to a cancelled loser is passed on.** A `First` loser
  can be granted what it waits for and cancelled before it resumes: a `Lock` it acquired
  (free when awaited, so during its hop, or released to it in the callback that decides
  the `First`), or a `Queue.get()` (`put()`) woken by a `put` (`get`). cocotb throws
  `CancelledError` into it: the Lock stays held by the cancelled helper and nobody can
  acquire it again, and the Queue wake is lost, so the next getter never gets the item
  (both seen in experiments). simjig releases the lock to the next waiter and passes the
  wake on (`sync::a_lock_taken_by_a_cancelled_first_loser_is_released`,
  `sync::a_queue_wake_of_a_cancelled_first_loser_goes_to_the_next_getter`,
  `sync::lock_is_granted_in_arrival_order_and_passed_on_by_a_cancelled_waiter`). A loser
  cancelled while still waiting takes nothing in either (`sync_losers`).

### Simulation end
The simulation ends when no timed callback and no `--timing` event is left. A test that
awaits `ReadWrite` from inside `ReadWrite` with nothing pending, and no clock running,
ends the simulation (cocotb reports `SimFailure`). This was seen in an experiment and is
not in a golden.
