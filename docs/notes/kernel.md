# Kernel notes (agent "kernel", branch `w1/kernel`)

Code: `crates/simjig/src/kernel.rs` and `crates/simjig/src/kernel/`. Tests:
`crates/simjig/tests/kernel_tests/` (one binary) plus unit tests per module.

## Normative loop (from the cocotb goldens, overrides DECISION §1)
Per timestamp, as in cocotb 2.1's `verilator.cpp` with trusted inertial writes:
1. timed region: `set_time`; wake `NextTimeStep`, run; wake `Timer`s due now and log the
   kernel clocks' edges (writer = clock, declaration order), run;
2. delta loop: `eval`; apply the write log; fire the value-change groups whose edge
   happened, one at a time, running tasks after each (wake order: the conformance README,
   "Callback order", which is normative); stop after an iteration that applied nothing;
3. `ReadWrite` region: wake, run, apply + compare + run; if anything was applied, go back to 2;
4. `ReadOnly` region: wake, run; any write is a failure;
5. advance to min(timer wheel, clocks, `next_time_slot()` if `events_pending()`).

Consequences pinned by the goldens and reproduced byte for byte by
`tests/kernel_tests/conformance.rs` (10 of the 13 scenarios: all except `two_clocks`,
`cdc_stream`, `fifo_stream`, whose DUTs need more replica work; the coincident-edge rule
of `two_clocks` has its own test in `phases.rs`):
- after `rising(clk)` on a kernel clock, flops still show old values (the edge fires
  between the apply and the eval that clocks the flops); design-driven edges see post-eval
  values;
- Timers resume before the timestep's edges: they win ties in `first!` and `with_timeout`;
- writes are deposits applied after the next eval; a write after an edge is not captured by
  it; reading your own write returns the old value.

`main` runs first at t=0 before any eval (cocotb's start-of-simulation), so reads there
show the model's initial values.

## Decisions and deviations (report to the director)
- **WriteConflict**: two *different* writers writing *different* values in one delta (cocotb
  has no such check; last write wins). Agreeing writers are accepted. Same-writer rewrites:
  last wins.
- **DELTA_MAX = 64** counts evals per timestep (including the ReadWrite loop-backs).
- **ReadOnly or ReadWrite awaited in ReadOnly** panics (→ `TaskPanic`); `ReadWrite` awaited
  in a `ReadWrite` region resumes in the next region (the next timestep if nothing was
  written; with nothing scheduled, the run fails `Starved`, where cocotb ends the sim).
  `Timer` of zero ticks panics. `rising`/`falling` on a multi-bit signal panic (use
  `changed`).
- **Starved vs Deadlock**: no next timestep and `main` unfinished. Deadlock if every
  unfinished task waits only on wait lists (task join, Event, Lock, Queue); else Starved.
  Both list each task with its registrations. Added `TimeLimit` (`Sim::set_time_limit`)
  because clocks make a hung test run forever; the swarm needs it.
- **Task ownership**: the `Task<T>` handle owns the task (drop = cancel); `detach()` gives
  it to the parent task, which cancels it when it completes or is cancelled; `scope` is
  spawn + await. Awaiting a task returns `T` directly: a child panic fails the whole run,
  so there is no `Result`.
- `Failure` has an extra `precision` field to print times in physical units.
- `FakeModel` is public API, not behind a `test-util` feature: it is useful for testing
  testbench logic without Verilator, and gating it would leave `Sim` without a public
  constructor until the model backend lands (`Sim::from_backend` is `pub(crate)` for it).
- `RawSig` is `!Send` (single-threaded kernel), and the module root is `src/kernel.rs`
  (no `mod.rs`, per the Rust rules), next to my owned `src/kernel/`.
- `lib.rs` got `#![deny(unsafe_code)]`; `kernel/waker.rs` opts out with `#![expect]`. The
  model agent's `model/raw.rs` will need the same opt-out.
- The workspace denies `expect`; the kernel uses an internal `OrInvariant::or_invariant`
  (panics with the violated invariant) instead of one `#[expect]` per call.
- `join!` and `first!` box each operand; `first!` requires one output type (returns it).
  Each operand runs in an internal helper task, as in cocotb (`kernel::combinator`; the
  conformance README, "Callback order").

## Why not simpler (constitution rule 5)
- **Raw waker (`unsafe`)**: the data word packs `sim_gen:32 | task index:16 | gen:16`; no
  allocation, `Send + Sync` trivially. `std::task::Wake` would need an `Arc` per task and
  an `Arc<Mutex>` run queue. Cross-thread wakes are detected via a process-wide
  `Mutex<Vec<(sim_gen, ThreadId)>>`, consulted only when the waking thread is not running
  that simulation. Task slots are limited to 65 536 live tasks by the packing; a wrapped
  16-bit generation can at worst cause a spurious poll.
- **Triggers store `cx.waker()`**, not the task id, so foreign combinators that poll with
  their own waker work (tested in `phases.rs`).
- **One `RefCell` per piece of state** (tasks, ready queue, waits, write log, model,
  recorder, failure) so no borrow is held while user code (polls, drops, wakers) runs.

## Verified
- `pixi run cargo xtask ci`: all steps pass (59 nextest tests, 3 doctests).
- Miri: `MIRIFLAGS=-Zmiri-strict-provenance cargo +nightly miri test -p simjig` — all 14
  unit, 45 integration (swarm reduced to 8 seeds) and 3 doctests pass. Installed for this:
  `rustup toolchain install nightly --profile minimal --component miri,rust-src`
  (nightly 2026-10-16).
- Mutation check: making `Registration::drop` skip deregistration fails the swarm and the
  cancellation tests.

## Not verified / open
- No real Verilator backend yet: `ModelBackend` is untested against Verilator, and the
  5 ns per-access budget (DECISION §4 Q2) is not measured.
- Panic *location* of a `TaskPanic` is not captured (the default hook prints it on
  stderr); `location` is set for kernel-detected failures (writes) only.
- No `events.jsonl` dump, `simjig::log!`, or repro line yet; the recorder keeps the last
  4096 records and hashes all of them.
- No CHANGELOG entry or API snapshot (constitution rule 4); there is no CHANGELOG file yet.

# kernel2 (agent "kernel2", branch `w3/kernel2`)

## Clock control from inside a run
`simjig::clock(sig, period) -> ClockHandle` declares a stopped clock (or reconfigures the
signal's stopped clock); the handle has `phase(d)`, `high_for(d)`, `start()`, `stop()`,
`is_running()`, `signal()`. `Sim::add_clock` now returns the `ClockHandle` too, so a pre-run
clock can be stopped and restarted. Clocks moved from `Sim` into the kernel
(`Kernel::clocks`, declaration order).
- `start()` at `t` behaves like a clock added at `t`: the level due at `t` is written at
  once through the write log (writer: the clock) and later edges follow the schedule from
  `t`. From `main` at t=0 this is byte-identical to `add_clock` (same trace hash, tested);
  from a task woken by a Timer/NextTimeStep the first edge shares the timestep's first eval
  with the other clocks' edges.
- `stop()` cancels edges not yet written; the level stays. Restarting keeps the clock's
  place in the coincident-edge order (tested at a coincident edge after a restart).
- Misuse panics at the caller (`#[track_caller]`, panics raised outside closures so the
  location is the user's line): start twice, configure a running clock, a non-1-bit signal,
  invalid period/high time, redeclaring a running clock, `start()` in ReadOnly.
- `clock` takes `impl Into<RawSig>`. The typed handles (`In<Bit>`, signal.rs, not mine)
  need `impl From<In<Bit>> for RawSig` for `simjig::clock(dut.clk, ns(10))` to compile
  as written; today it is `clock(dut.clk.raw(), ns(10))`.

## `--timing` safety
- `next_time_slot` is asked only of models whose `events_pending()` is true
  (`Models::next_event`); the next timestep is the minimum of timers, clocks and every
  model's slot, so time never passes a pending slot; `run_loop` asserts time strictly
  increases.
- A model slot equal to now after the deltas settle (a pending `#0`) is evaluated again as
  another delta of the same timestep (counts toward `DELTA_MAX`; a `#0` loop is a
  `DeltaLimit`). In 5.052 `nextTimeSlot()` returns the queue head before `#0`s, and the
  generated eval resumes `#0` processes itself, so this is defensive. A slot before now is
  `ModelFatal` ("missed a time slot").
- `FakeModel` is now as strict as Verilator: `next_time_slot` with nothing pending panics,
  an eval with a pending event before now fails, and model code can schedule events
  (`FakeState::schedule`, including `#0` at now). `FakeModel::at` takes `FnOnce`.
  Tests: `kernel_tests/timing.rs` (design `#5` clock + 7-tick kernel clock + Timers, the
  spike's merged loop; two timed models + a kernel clock; events running out; `#0`; `#0`
  loop; event in the past).

## TaskPanic location
`Failure.location` is filled for task panics (poll and drop). A process panic hook,
installed once and chained to the previous hook, stores the latest panic location in a
thread-local while a `Sim::run` is on the thread; the poller takes it after
`catch_unwind`. Not removed after the run (deviation from "restored afterwards"):
`cargo test` runs tests on several threads, and `take_hook` at the end of one run would
remove a hook installed meanwhile (the harness installs its own once). Outside a run the
hook only forwards.

**API change**: `Failure.location` is `Option<&'static SourceLocation>` (was
`Option<&'static Location<'static>>`). A panic hook's location borrows the panic, so it
cannot be stored as a `&'static Location`, and std has no constructor for one; without
`unsafe` the only option is our own type. Locations are interned (one leaked record per
distinct position, bounded by the source). `SourceLocation` has `file()/line()/column()`,
the same `Display` as std, and `From<&'static Location<'static>> for &'static
SourceLocation`. Harness `src/` compiles unchanged; one line in
`tests/harness_tests/reports.rs` needed `Some(location.into())` (outside my ownership;
changed so CI passes, flagged for the director). The harness's own panic-location
workaround still works and now agrees with the kernel's.

`Failure::help() -> Option<&str>` exposes the advice; the harness can stop parsing
`Display` (`harness/report.rs::help`).

## Multiple models
`Sim::from_fakes(iter) -> Result<Sim, ModelSetError>` and crate-internal
`Sim::from_backends(Vec<Box<dyn ModelBackend>>)`; `Sim::signal_in(model, name)`,
`Sim::model_count()`, `RawSig::model()`. `Sim::signal(name)` looks only in model 0.
- `kernel/models.rs`: all models' slots form one flat range (model 0 first), so the write
  log, waits and clocks are unchanged; `RawSig` is `(sim, model, local slot)` and resolves
  to the flat slot in `RawSig::with`.
- Every delta evals all models in declaration order (stopping at the first failing one),
  then applies the log; value-change groups cover all models and fire in priming order
  across models (tested; order rules: the conformance README, "Callback order"). Models must share one precision (`ModelSetError::PrecisionMismatch`).
- Failure messages name the model ("model 1: ...") only when there are several.
- The trait documents that each backend switches `Verilated::threadContextp` (and its
  `$urandom` state) itself in `eval` and any call that runs model code.
- Diagnostics use each model's own slot names, so same-named signals of two models look
  alike in messages (the `RawSig` Debug shows the model). A per-model name prefix would
  need a backend method; not added.

**Integration with `w2/model`** (checked by a trial merge in a scratch worktree): three
one-line fixes. `model/bind.rs`: `RawSig::new(sim_gen, slot)` -> `RawSig::in_model(sim_gen,
0, slot)`, `raw.slot()` -> `raw.local_slot()`; `signal.rs`: `k.model` -> `k.models`.

## Write-path cost
Before: `write_u64` heap-allocated a buffer, each new log entry allocated a writer `Vec`,
and applying the log copied every value into a new `Vec`. Now: no allocation in steady
state (entries hold the first writer and values of <= 2 words inline, agreeing writers go to
a side list, buffers are recycled across deltas), the per-slot epoch table is 8 bytes, the
sole-writer rewrite skips the checks and `memcmp`, and failure paths are `#[cold]`.
Semantics unchanged (unit test `the_log_keeps_the_last_value_and_names_a_disagreeing_writer`
plus the existing conflict tests).

`kernel_tests/bench.rs` (ignored): 512 32-bit signals, one call per signal per timestep,
400 timesteps; median per-timestep ns per call, best of 6 runs alternating old and new
builds on the same (heavily loaded, load average about 20 on 12 cores) machine:

| op | before | after |
|---|---|---|
| harness noop | 0.65 | 0.65 |
| `width()` (handle resolution only) | 2.52 | 2.52 |
| `read_u64` | 5.29 | 5.29 |
| `write_words` (1 word) | 12.86 | 5.53 |
| `write_u64` | 22.3 | 5.62 |
| whole run per write (eval, apply, recorder, FakeModel) | 61.9-71.4 | 38.9-39.4 |

The model agent's `measure_typed_access` on a trial merge went from 6.7-7.6 to 6.1-7.4 ns
for the same-slot rewrite case, but its numbers swung 2x between runs on the loaded machine
(efficiency cores); not conclusive. Writes now cost about what reads cost (+0.3 ns here);
whether both are under 5 ns on an idle machine is **not verified**. The remaining floor is
the shared handle resolution (thread-local + `RefCell` borrow, about 1.9 ns), which needs
`unsafe` or a different current-kernel scheme to shrink.

## Verified
`pixi run cargo xtask ci` (see the final report). Mutation: removing the `#0` re-eval fails
both zero-delay tests; without the panic hook `Failure.location` is `None` and the location
tests fail.
