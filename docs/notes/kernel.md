# Kernel notes (agent "kernel", branch `w1/kernel`)

Code: `crates/simjig/src/kernel.rs` and `crates/simjig/src/kernel/`. Tests:
`crates/simjig/tests/kernel_tests/` (one binary) plus unit tests per module.

## Normative loop (from the cocotb goldens, overrides DECISION §1)
Per timestamp, as in cocotb 2.1's `verilator.cpp` with trusted inertial writes:
1. timed region: `set_time`; wake `NextTimeStep`, run; wake `Timer`s due now and log the
   kernel clocks' edges (writer = clock, declaration order), run;
2. delta loop: `eval`; apply the write log; compare watched slots against the previous
   comparison and wake in registration order; run tasks; stop after an iteration that
   applied nothing;
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
- `join!` boxes each operand and `first!` requires one output type (returns it); both
  poll left to right.

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
