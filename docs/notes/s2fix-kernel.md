# s2fix-kernel notes (branch `fix/s2fix-kernel`, from `stage/2`)

Fixes for the stage 2 review's kernel and model findings, plus three DPI hooks and the
time display change the director asked for. Each fix has a test that failed (or did not
compile) before it.

## Fixes
| Finding | Change | Proving test |
|---|---|---|
| Detached task outlives its scope | `TaskEntry` keeps its ancestor chain (`Rc<[Key]>`); `detach` attaches to the nearest ancestor that is `Pending`/`Polling`; the owner is stored so `free_task` unlinks the right list | `cancel::a_task_detached_after_its_spawner_finished_dies_with_the_scope` |
| Cleanup panic masks the real one | `drop_guarded`: if `thread::panicking()` on entry, print the second panic, don't record it, restore the first panic's location | `cancel::a_cleanup_panic_while_unwinding_does_not_mask_the_first_panic` |
| `TimeLimit` lacks waits | `TimeLimit { limit, blocked }`, same list as `Deadlock`, shown in `Display` and the harness report | `failures::time_limit_stops_a_run_that_would_go_on_forever`, snapshot `time_limit_with_the_run_dir` |
| `let _ = spawn(..)` | warning + `TraceKind::DroppedUnpolled`; `spawn` docs show `spawn(..).detach()` | `cancel::a_handle_dropped_before_its_task_ever_ran_is_recorded`, `spawn` doctest |
| DeltaLimit empty for ReadWrite loops | separate `READ_WRITE_MAX` and `FailureKind::ReadWriteLimit { limit, waiters }` | `failures::read_write_loop_backs_have_their_own_higher_limit`, `failures::read_write_limit_names_the_task_and_line_that_keep_looping_back` |
| Trigger misuse reports `trigger.rs` | constructors `#[track_caller]` capture the location; `panics::panic_at` blames it | `phases::trigger_misuse_points_at_the_line_that_made_the_trigger` |
| Free triggers take only `RawSig` | `impl Into<RawSig>` | `signal::tests::the_free_triggers_take_a_writable_typed_handle` |
| `simjig-bits` needs a direct dependency | `pub use simjig_bits as bits` | crate-root doctest |
| FakeModel closure panics escape `Sim::run` | `Models::eval` catches them, `ModelFatal` naming message and line | `failures::a_panicking_fake_model_closure_is_a_model_fatal` |
| Unchecked pointer alignment | `raw::validate` checks alignment per storage class | `model::raw::tests::validate_rejects_a_member_misaligned_for_its_storage_class` |
| No Miri entry point | `cargo xtask miri` (CONTRIBUTING.md) | the run itself |
| WriteConflict nested backticks | fixed on the harness side (fix/s2fix-harness 8c5874d); my kernel-side commit is reverted | harness `write_conflict` snapshot |

Director requests:
- `FailureKind::DpiPanic { message }`, from `ModelError { kind: ModelErrorKind::DpiPanic }`
  (`ModelError::fatal` / `ModelError::dpi_panic`; `model/verilator.rs` changed only to use
  `ModelError::fatal`). Test: `kernel::sched::tests::a_dpi_panic_is_its_own_failure_kind`.
- `state::enter_dpi_call(import) -> DpiCallGuard` and `state::in_dpi_call()`. While a guard
  is alive, `with_current` and `try_current` (signals, triggers, `spawn`, `now()`, sync
  primitives, duration constructors) panic: ``DPI-C import `x` used <what>: DPI functions
  run inside eval; they can't access signals or await``. Test:
  `kernel::state::tests::kernel_entry_points_refuse_to_run_inside_a_dpi_call`. For the dpi
  module: replace `forbid_in_call` and its `in_call` cell with `enter_dpi_call` around the
  implementation call; `try_current` now takes a `what` argument.
- `kernel::panics` is `pub(crate)`: call `panics::forget()` before `catch_unwind` and
  `panics::take()` after to get the panicking line.
- `TimeDisplay` prints in the largest exact unit (`1 us`, `250 ns`, `1250 ns`, `0 ps`), the
  same rule as the harness's `human_time`, which can now delegate to it.

## Decisions and why
- **Ancestor chain, not reparenting on free.** Finding the children of a freed task would
  need a scan or a children list per task; the chain is one small `Rc<[Key]>` per spawn,
  and stale keys are detected by the slab's generations.
- **Unpolled drop: warning, not failure.** Drop is the one cancellation mechanism
  (DECISION §1), and dropping a fresh handle is also how a testbench abandons work (the
  swarm tests do it constantly). The warning is skipped when the drop is part of a larger
  cancellation (end of run, the dropping task is itself being cancelled, unwinding), so it
  fires for the `let _ = spawn(..)` shape. No `spawn_detached`: `spawn(..).detach()` is
  one obvious way, documented in `spawn`.
- **`READ_WRITE_MAX = 10 000`.** cocotb on Verilator has no limit: its main loop
  (tests/conformance/README.md, "The time-step loop", step 3) returns to the deltas after
  every `ReadWrite` that wrote, forever. A ReadWrite round is a deliberate testbench step,
  so many are legitimate; 10 000 is far past any real test and fails a runaway loop in
  milliseconds. `DELTA_MAX` (64) now counts evals between ReadWrite rounds, so an
  oscillation inside one settle is still caught early; total evals per timestep are
  bounded by `(READ_WRITE_MAX + 1) * DELTA_MAX`. A `#0` model loop (no ReadWrite waiter)
  still counts against `DELTA_MAX`, and its message now says so instead of blaming
  ReadWrite. This supersedes `docs/notes/kernel.md` ("DELTA_MAX counts ReadWrite
  loop-backs").
- **Timer::new stays `const`.** A `Timer` built inside a DPI call is inert data; awaiting
  it goes through `try_current` and is refused there.
- **Misuse location.** The panic hook records the location `panic_at` set; the default
  hook still prints `trigger.rs`, so the message also names the user's line.

## API changes (rule 4; there is no CHANGELOG in the repository)
- `FailureKind::TimeLimit` gains `blocked`; new variants `ReadWriteLimit`, `DpiPanic`
  (exhaustive matches on `FailureKind` break).
- New `READ_WRITE_MAX`, `TraceKind::DroppedUnpolled`, `simjig::bits`.
- `rising`/`falling`/`changed`/`cycles` take `impl Into<RawSig>`.
- `DELTA_MAX` semantics: evals between ReadWrite rounds, not per timestep.
- Time strings in failures and messages change (`t=1 us` instead of `t=1000000 ps`).
- Trace hashes change only for runs that drop a never-polled handle.

## Files outside my list
`harness/report.rs` (match arms for the new and changed `FailureKind` variants, a shared
`blocked_lines`), `tests/harness_tests/reports.rs` (the `TimeLimit` field) and its
snapshot, `tests/e2e_dut/fatal_tb.rs` (`t=10 ns`), `model/verilator.rs`
(`ModelError::fatal`), `tests/kernel_tests/swarm.rs` (new variants). All are needed for
the workspace to compile and the gate to pass. The harness branch made the same snapshot
and `fatal_tb` changes for its own time display.

## Verification
- `pixi run cargo xtask ci`: all checks passed (nextest 517 passed, 7 skipped).
- `pixi run cargo xtask miri`: lib 45 passed (141 s), kernel_tests 73 passed, 1 ignored
  (710 s), on a loaded machine. Without `-Zmiri-ignore-leaks` the only errors were the
  backend tests' fake vtables leaked into thread-locals (`model/verilator.rs` tests).
- Not verified: Linux; the DPI hooks against the real `dpi` module (not merged here);
  `tests/abi_counter.rs` under Miri (it links C++).
- The Miri toolchain is the unpinned `nightly` (rustc 1.101.0-nightly 2026-10-16 here).
