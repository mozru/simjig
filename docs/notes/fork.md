# Notes: multiple DUTs and `fork()` checkpoints

Code: `crates/simjig/src/harness/multi.rs` (tuple DUTs, `Lockstep`, `Mirror`),
`crates/simjig/src/harness/dut.rs` (`DutModel::{load, bind}`, `LoadedModel`),
`crates/simjig/src/fork.rs` + `fork/{sys,wire}.rs`, the attribute in
`crates/simjig-macros/src/test_attr.rs`. Tests: `crates/simjig/tests/multi_dut/` (11) and
`crates/simjig/tests/fork_checkpoints/` (16), plus `fork::wire` unit tests (3).

## 1. Several DUTs in one test

```rust
#[simjig::test]
async fn t(ctx: &TestCtx, a: inv::Dut, b: flop::Dut) { ... }
```

**Choice: the attribute, not a builder.** The macro turns 2 to 4 by-value parameters into
the DUT type `(A, B, ...)` and the closure pattern `(a, b, ...)`; `simjig` implements
`DutModel` for those tuples. Why:
- the harness flow (seed, Verilator seed, timeout, report, run dir) is unchanged: the
  runner still sees one `DutModel`;
- the tuple impl *is* the explicit builder for code without the attribute:
  `<(fatal::Dut, inv::Dut)>::instantiate(&options)` then `::create(&mut sim)`
  (`multi_dut/designs.rs` does this);
- a single DUT expands exactly as before (snapshots unchanged); 5+ DUTs is a spanned error.

**Seam.** `DutModel` gained two defaulted methods, so every existing impl compiles:
- `load(&ModelOptions) -> Result<LoadedModel, Failure>`: the model alone (default: a
  `ModelFatal` naming the type and saying to implement `load`/`bind`);
- `bind(&mut Sim, model: usize) -> Result<Self, Failure>` (default: `create` for model 0).

`LoadedModel` is an opaque box around the kernel's `ModelBackend` (`from_fake` for
hand-written DUTs). Generated `Dut`s get both through `dut_model!`; `model::bind` got
`load` and `create_in(sim, model, meta, bind)`, and `ModelBinder` now carries the model
index (it was hard-coded 0). Each model keeps its own `VerilatedContext` (one
`simjig::Runtime` per handle), so `$time`, `$finish`, `$fatal` and `$urandom` stay
per model; `a_fatal_names_the_model_that_raised_it` puts the fatal model first, so the
thread's context after construction is the *other* model's.

**Same seed for every model.** All models get the same `ModelOptions`, so two instances
of one design draw the same `$urandom` values, which lockstep needs.

**Lockstep.** `Mirror::new(a.en, b.en).set(v)` writes both inputs in one delta.
`Lockstep::new().output(old.count, new.count).compare(name, f, g)` holds typed
comparisons (`Out<T>` pairs need the same `T`; `compare` takes any `S: PartialEq + Debug`);
`check()` runs all of them, counts the cycle, and returns `Divergence { cycle, time,
precision, diffs: Vec<FieldDiff> }` listing every differing sample (reusing the codegen's
`FieldDiff`); `step()` panics with it (a `TaskPanic` at the user's line). Tested against
`multi_dut/counter_rev2.sv` (same `counter` module, 5 -> 7), built as a second test model
with its own prefix: divergence at cycle 5, `counter.count: 8'h06 != 8'h07`.

## 2. `fork()` checkpoints

```rust
let branches = simjig::fork::run_from_checkpoint(ctx.seed(), n, |branch| async move {
    let mut rng = branch.rng("stim");
    /* stimulus */
    Ok(value)                 // T: BranchValue
}).await?;                    // ForkError if it could not fork
let values = branches.into_values(); // panics listing failed branches (index + seed)
```

**Choice: `run_from_checkpoint`, not a bare `checkpoint()`.** A bare checkpoint that
returns "parent" or "child" lets the child run the rest of the test and fall back into
the harness, whose main thread no longer exists in the child. The structured form keeps
the child inside the branch body and gives the parent typed results.

**Mechanism.** At the call (in main's poll) the parent checks the rules, then for each
branch: runs `ForkHook::before_fork`, flushes Rust and C stdio, makes a `UnixStream` pair,
forks, reads the child's report to EOF and reaps it with `wait4`. Branches run one at a
time, in index order (deterministic; no fan-out across cores, which nextest already
does across tests). The call never suspends in the parent, so the kernel records nothing:
`the_parent_matches_a_run_that_never_forked` compares trace hash, end time and result
of a run with and without a checkpoint.

In the child: `ForkHook::in_child`, then a detached sentinel task `main.fork_sentinel`
(a future that is always pending), then the body is awaited inline.
- Body returns `Ok(v)` or `Err(failure)`: the report is written from inside main's poll,
  stdout/stderr are flushed, `_exit(0 | 1)`.
- Anything else (task panic, `ModelFatal`, `TimeLimit`, `Starved`, ...): the kernel ends
  the child's run and drops every task; the sentinel's `Drop` reads the kernel's
  `Failure` (still set: `Sim::run` takes it after dropping tasks) and exits. A panic
  cannot be reported from the body's own frame: the panic unwinds through it before the
  kernel records the failure (with the panic location), hence the sentinel.
- Child killed by a signal (`abort()`, a Verilator internal abort) or exiting without a
  report: `BranchError::Died(ChildExit::Signaled(6))`.

The report: `wire.rs`, tag byte + `BranchValue` encoding (fixed-width little-endian, `u64`
lengths, UTF-8). `BranchValue` is implemented for `()`, `bool`, integers, `String`,
`Bit`, `Bits<N>`, `SimTime`, `Vec`, `Option` and tuples up to 3. A failure crosses as
`RemoteFailure { kind (name), time, task, location, message, text }`: `FailureKind` has no
encoding of its own, and adding one is a kernel change.

**Rules enforced** (spikes.md §1):
| Rule | Enforcement | Test |
|---|---|---|
| Unix only | `ForkError::Unsupported` elsewhere (cfg) | not run (no non-Unix CI) |
| <= 2 threads | `proc_pidinfo(PROC_PIDTASKINFO)` on macOS, `/proc/self/status` on Linux; `ForkError::Threads` names nextest | `a_third_thread_refuses_the_fork` |
| `threads(1)` | the runtime sets it; a pool would fail the thread check | every fork test loads a real model and forks |
| quiet point | caller is `main`, no other unfinished task | `another_running_task_refuses_the_fork`, `a_task_other_than_main_cannot_fork` |
| `_exit` in the child | `finish_child`, both paths | `failures.rs` (every ending), `buffered_output_is_printed_once` |
| no double output | stdio flushed before fork | `buffered_output_is_printed_once` (3 copies without the flush; checked) |
| open trace | `ForkHook` (flush before, detach/reopen in child) | `hooks_run_before_each_fork_and_in_each_child`, `a_failing_hook_*` |
| state preserved | parent untouched | `branches_diverge_and_the_parent_is_unaffected`, `the_parent_matches_a_run_that_never_forked` |

Branch seeds: `Seed::new(root.rng("fork.branch.<i>").next_u64(), root.source())`; the
root seed in the failure report reproduces every branch
(`branch_seeds_are_distinct_and_follow_the_root_seed`).

**Measured** (release, Apple M6, macOS, load average 30-39 on 12 cores, 3 runs of
`cost::copy_on_write_shows_in_the_branch_cost --no-capture`, counter model loaded,
32 MiB of ballast touched before the fork):
| | fork latency | wall to reap | child CPU | minor faults |
|---|---|---|---|---|
| idle branch | 0.18-0.50 ms | 0.40-0.66 ms | 0.17-0.22 ms | 141 |
| branch rewriting 32 MiB | 0.26-0.34 ms | 2.8-3.0 ms | 2.4-2.5 ms | 2189 (+2048 = 32 MiB / 16 KiB pages) |

So about 0.5 ms per branch plus about 0.08 ms per MiB the branch writes, matching the
spike (0.36 ms, 0.1 ms/MiB). The fault count is asserted (>= 2048 extra); times are only
printed.

## Limits (also in the `//!` docs)
- **Unix only**; only macOS and Linux can count threads (other Unixes get `Unsupported`).
- **nextest only** for forking: under multi-threaded `cargo test` every call returns
  `ForkError::Threads`. `cargo test -- --test-threads=1` also passes the check.
- **No DPI exports in a multi-model simulation**: Verilator names an exported function's C
  symbol after the SV function, not the prefix, so two builds collide at link time, and
  two instances of one build pick the instance by `svSetScope`, which simjig does not
  switch per model. DPI imports are per model.
- Multi-model: one shared precision; at most 4 DUTs per test; one seed for all models.
- Branches cannot spawn tasks that outlive the body (the child exits when the body
  returns); a branch's `Drop` cleanup does not run.
- **No nested checkpoints**: inside a branch the call returns `ForkError::Nested`, checked
  before every other rule ("in a branch" is "this process holds the socket to its
  parent", so it needs no new state). Nesting is refused rather than supported because
  the sentinel, the channel, the armed hooks, the failed-branch record and the waves file
  names would each need a level. Because of it, a branch trace file always has exactly
  one `.branch-<j>` level.
- **No fork inside a combinator** (`ForkError::InCombinator`): an operand of `join!`,
  `first!` or `with_timeout` is a helper polled inside `main`, not a task, so the
  quiet-point check alone would let it fork. A sibling operand would then keep running in
  every branch, and a `first!` sibling that won in a child would cancel the body and let
  the child run the rest of the test as the parent.

## Changes outside the fork module
- `model/bind.rs`: `load`, `create_in`, `ModelBinder::model`, `dut_model!` emits `load`
  and `bind`; `create` = `create_in(.., 0, ..)`. Additive; needed because only the model
  layer can build a backend without a `Sim`.
- `crates/simjig/build.rs`: one more test model (`counter_rev2`).
- `tests/harness_ui/bad_params.{rs,stderr}`: two DUTs are now valid; the case is five.
- Root `Cargo.toml`/`Cargo.lock`: `libc = "0.2.191"` (already in the lock as a transitive
  dependency); `simjig` depends on it for Unix only. Why not std: `fork`, `_exit`, `wait4`
  and `proc_pidinfo` have no std wrapper, and hand-written `rusage`/`proc_taskinfo`
  layouts would be a second copy of libc's.
- Traces: an open FST registers a `ForkHook` (`model::trace`), which continues the
  branch's waves in `<stem>.branch-<i>.fst`; see `docs/notes/waves.md`.
- `fork.rs` reads kernel fields (`phase`, `main`, `current`, `tasks.unfinished()`,
  `failure`) through `state::try_current`; no kernel code changed.
- Contradiction with spikes.md: none found. DECISION.md says "`fork()` checkpoints (Unix,
  `--threads 1`)"; the context-level `threads(1)` is what matters (spikes §1), and is set
  by the runtime.

## Not verified
- Linux (all runs on osx-arm64): the `/proc/self/status` path, `ru_maxrss` in KiB, fork
  latency growing with RSS.
- Non-Unix builds (`cfg(not(unix))` code was not compiled).
- Miri: `fork/sys.rs` is FFI only.
