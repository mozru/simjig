# Stage 2, review cycle 2 fixes (agent "s2c2fix", branch `fix/s2c2fix`)

## 1. The UI test on a cold target
trybuild's nested cargo runs in `<target>/tests/trybuild` but below the repository, so it
reads `.cargo/config.toml` and its `SIMJIG_TEST_MODELS=1`; simjig's `build.rs` then
verilated all 15 test models into that second target directory.

- Fix: `build.rs` skips the models when `OUT_DIR` lies under a `tests/trybuild`
  directory (`in_trybuild`). Chosen over the UI test setting `SIMJIG_TEST_MODELS=0`
  (which would win over `[env]`): that needs `unsafe { std::env::set_var }` in a test
  binary whose other tests read the environment on other threads (under `cargo test`),
  and it only works for runs that go through that one test function. trybuild 1.x
  always uses `<target>/tests/trybuild` (`trybuild-1.0.122/src/cargo.rs`).
- Test: `ui::signature_errors_point_at_the_offending_tokens` removes every nested
  build-script run directory whose `output` has `cargo::rustc-cfg=simjig_test_models`
  (so cargo reruns that script), runs the cases, and asserts that none reappears. It
  failed before the fix (two such directories, check and build).
- Backstop: a nextest override gives `binary_id(simjig::harness_tests) & test(/^ui::/)`
  `slow-timeout = { period = "60s", terminate-after = 5 }` (default 30 s x 4).
- Measured, `rm -rf target/tests/trybuild` then the UI test alone: **55.2 s** before
  (nextest flagged it SLOW; it verilated into `target/tests/trybuild/simjig-cache`,
  12 MB), **5.4 s** after, with no `simjig-cache` there.

## 2. Default timeout at coarse precisions
`default_timeout_ticks`: `10 * 10^(-3-e)` ticks for `e <= -3`, else
`ceil(10 / 10^(e+3))`, at least 1. Test:
`runner::the_default_timeout_is_10ms_rounded_up_to_whole_ticks` (1 s and 10 ms per tick
give 1 tick; it got 10 before). The default-timeout tests' bodies now end after 20 ms, so
a missing default fails them instead of hanging into nextest's kill.

## 3. `with_timeout` ties
cocotb 2.1: `with_timeout(t, d)` is `select(Timer(d), t)`; `_wait` starts its waiter
tasks with `_start_next` in reverse, so the timeout's waiter runs, and arms its Timer,
first. Each GPI callback (`GPITrigger._react`) runs the whole event loop before the next
callback fires.

Two changes, both needed (each alone fails the `timeout_ties` replay):
- `with_timeout` polls the timer before the future, so the timeout's Timer registers
  first, and checks it first.
- **Executor semantics** (`sched::step`): Timers due at t fire one at a time in
  registration order, with `run_ready` after each. Before, all fired together and tasks
  they woke ran after the last Timer's task. Without this, the timer-first order would
  make a spawned task whose own Timer was armed *before* the call time out, where cocotb
  returns its result (recorded: `timeout_ties` at 54.999 ns, `fired`).
  Why not simpler: the timer-first order alone gets every case of the brief right but
  regresses that one; deciding it inside `with_timeout` would need the kernel's fire
  order of unrelated Timers. Firing one Timer at a time is what cocotb does and costs one
  extra `run_ready` per due Timer (each wakes a task anyway). Clock edges are still logged
  before any Timer task runs, so the write-log order is unchanged. Every existing golden
  replay (kernel and e2e) and the swarm tests pass unchanged; the trace hashes of tests
  with same-time Timers may differ from before (none is pinned).
  Pinned by `phases::each_timer_runs_what_it_wakes_before_the_next_timer_due_then`
  (failed before: `old, young, woken`).

Golden `timeout_ties` (flop, `tests/conformance/tb/test_timeout_ties.py`): Timer,
sub-coroutine and just-started task ties at 15/20/25 ns time out; edge (35 ns) and
`ClockCycles` (45 ns) ties time out; one tick short (49.999 ns) fires; a task whose Timer
was armed first (54.999 ns) fires. `conformance-regen` rewrote only this golden; the other
13 are byte-identical.

## Nits
- Scheduler rules confirmed outside the goldens are in `tests/conformance/README.md`
  ("Further rules"), each pinned by a new or existing `kernel_tests` test. They are not in
  the root `README.md`, which is a quick start with no semantics section; the conformance
  README is the normative list (DECISION §0).
- `tests/harness_tests/duts/counter.rs` cannot have a `//!` doc: it is pulled in with
  `include!`, which rejects inner doc comments (E0753). Its `//` comment says so; the
  module doc comes from `include_dut!`'s `#[doc]`.
- FNV-1a: `kernel::recorder::Fnv`, used by the recorder, seeds and signatures (reference
  vectors tested); hashes are unchanged.

## Not verified
- The per-Timer order against cocotb beyond the two goldens that exercise it
  (`timeout_ties`, and indirectly the 13 others); edges and other same-time callbacks
  still wake together, which cocotb also runs one callback at a time.
- The ReadOnly-woken write, change-and-back and `--timing` Timer rules were not
  re-recorded against cocotb here; the README states them as confirmed by review.
