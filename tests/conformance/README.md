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

- **`First`, `Combine` and `with_timeout` resume their caller late.** They await each
  operand in a helper task (`_wait` in `_concurrent_waiters.py`, started with
  `_start_next`, so the helpers register before tasks started earlier but not yet run)
  and wake the caller through an internal event. When the deciding helper finishes, the
  caller is scheduled behind every task already queued: for `Combine` (and a `First`
  whose losers have all finished) that is one hop; `First` and `with_timeout` (`select`)
  first cancel a loser still waiting by scheduling it once more, and the caller is
  scheduled when that loser finishes, a second hop. So the caller resumes after the tasks
  the same callback woke directly (`combinator_hop` at 25 and 35 ns), and after the tasks
  those wake when a loser was cancelled: an `Event` set by such a task wakes its waiter
  before the caller of `First` (45 ns) and `with_timeout` (65 ns), after the caller of
  `Combine` (55 ns). simjig polls the operands inline and yields once, or twice when
  dropping the losers cancelled a pending wait (`kernel::combinator::hop`).

simjig mirrors this with one wait group per (signal, edge kind), ordered by the
registration that primed it (`kernel::waits`, "Value-change groups"), and a walk that runs
tasks to quiescence after each group that fires (`kernel::sched::fire_value_changes`).

### Further rules, confirmed but not in a golden
Each is pinned by a simjig test against `FakeModel` (`crates/simjig/tests/kernel_tests`).
- `NextTimeStep` waiters resume before that time step's `Timer` waiters (step 1 above;
  `phases::next_time_step_resumes_at_the_next_scheduled_time_before_timers`).
- `First` of edges that fire in the same comparison goes to the **leftmost** operand
  (`phases::first_of_coincident_edges_goes_to_the_leftmost_operand`).
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
Deliberate, each with the reason:
- **`Timer` of zero ticks** is rejected when it is first **awaited** (a `TaskPanic` naming
  the line that built it); cocotb raises `ValueError` when `Timer(0)` is **built**.
  `Timer::new` is a `const fn`, which can neither report a run failure nor put the call
  site in the message, and a `Timer` may be built before the run starts
  (`phases::trigger_misuse_points_at_the_line_that_made_the_trigger`).
- **Combinator operand bodies run in the caller.** cocotb runs each operand of `First`,
  `Combine` and `with_timeout` in its own helper task, so a multi-step operand (a
  coroutine that awaits several triggers) resumes at its helper's place in the event loop;
  simjig polls every operand when the caller is woken. The caller's own resumption point
  matches cocotb ("Callback order"); code *inside* an operand that runs between two of its
  awaits may run earlier or later relative to other tasks woken by the same trigger. No
  golden exercises it.

### Simulation end
The simulation ends when no timed callback and no `--timing` event is left. A test that
awaits `ReadWrite` from inside `ReadWrite` with nothing pending, and no clock running,
ends the simulation (cocotb reports `SimFailure`). This was seen in an experiment and is
not in a golden.
