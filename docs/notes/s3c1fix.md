# s3c1fix: Stage 3 review cycle 1 fixes (branch `fix/s3c1fix`, from `stage/3`)

One commit per fix; each names its proving test, which was run and seen failing before
the fix (the coverage and `TestCtx::precision` tests failed to compile: the API did not
exist).

## 1. Waves on failure kept only `waves.fst` (BUG)
`harness::replay` now moves every `waves*.fst` of the replay's root into the run
directory (`move_waves`; the names are parsed by `trace_identity`: `waves`, then an
optional `.model-<i>`, then one `.branch-<j>` per checkpoint level), writes one
`<stem>.gtkw` per file, and lists them all. `WavesReplay::files: Vec<WavesFile>` replaces
`fst`/`gtkw` (API change, CHANGELOG).

Fork-branch failures: `Branches::into_values` records the first failed branch (index and
its `RemoteFailure` time) in a thread-local (`fork::FailedBranch`); the runner takes it
after a failed run and forgets it at the start of every run. The branch's files come
first in the report (`waves:` names the branch file), and their `.gtkw` marker is at the
branch's own failure. The traced span is unchanged: a default window has no end, so the
branch child traces to its own end; `from` is computed from the checkpoint time, which is
never later than the branch's window start would be.

Tests: `waves_tests on_fail::every_model_of_a_failed_multi_dut_test_gets_its_waves`
(counter_fst + fifo_fst), `on_fail::a_failing_fork_branch_gets_the_waves_of_its_own_span`
(branch 1 fails 500 ns after the checkpoint; its file ends there, marker there).

## 2. A poisoned fork branch left a 0-byte FST (BUG)
`model::trace` keeps three facts apart: `sampling` (may dump), `file_open` (needs closing)
and `closed_by_destroy` (the C side refuses every trace call after FATAL/EXCEPTION).
- DPI poisoning (`Tracer::stop`): no more samples; the healthy C++ model closes the file
  with `trace_close` (in `Drop`, and in the fork hook's `at_child_exit`).
- FATAL/EXCEPTION (`Tracer::poisoned`, called from `VerilatorModel::outcome` whenever it
  poisons): Rust makes no trace call. In the parent `destroy` closes the file as before;
  in a fork child the hook calls the new `TracePort::destroy_before_exit`, which takes
  the handle out of the shared `live` cell and destroys the model (its teardown closes the
  trace). `RawModel::drop` now destroys only if `live` still holds the handle, so the
  handle is destroyed exactly once. Safe because the hook is the child's last code before
  `_exit` (`fork::imp::finish_child`).
- The C++ shim was not changed (not mine): `trace_close` still refuses a poisoned handle.

Tests: `waves_tests fork::a_branch_whose_dpi_import_panics_still_finishes_its_waves`
(file ends at the failing timestep) and `fork::a_branch_that_hits_a_fatal_still_finishes_its_waves`
(file ends one timestep before the `$fatal`, as documented). New test models in
`crates/simjig/build.rs`: `dpi_fst`, `fatal_fst`.

## 3. `help:` blamed the design for setup errors (RISK)
New `FailureKind::Setup { message, help: &'static str }` (and `ModelErrorKind::Setup`,
`ModelError::setup`, `LoadError::help()`). Used for: a DPI call without implementation,
a DPI call that cannot reach its instance (two instances of one build), two models whose
imports share a C name; every `LoadError` except `Create` (stale build, `NoTrace`,
`TraceOpen`, precision); `ModelSetError`; a DUT without `load`/`bind` in a tuple.
Snapshots: `dpi_tests failures::{a_missing_implementation_is_a_setup_failure_that_says_how_to_register_one,
two_instances_of_one_dpi_build_fail_with_the_workaround}`, `waves_tests
tracing::a_model_built_without_fst_says_how_to_enable_it` (model prefixes masked).

**Outside my files:** `kernel/sched.rs` gains one match arm mapping
`ModelErrorKind::Setup` to `FailureKind::Setup` (the mapping is an exhaustive match there;
no other way to carry the kind through). `kernel/failure.rs` gains the variant;
`kernel/backend.rs` the error kind. `tests/kernel_tests/swarm.rs` gains a match arm.

## 4. Two instances of one DPI build in lockstep (RISK)
Documented in `harness::multi` (module docs and `Lockstep`), `dpi` module docs, the
CHANGELOG, README and crate docs; the error message and its help give the workaround
("build the design a second time under another `Dut::new` name"). Same snapshot test.

## 5. Coverage counters could not be named or zeroed (RISK)
`Sim::{zero_coverage, write_coverage}` (ABI `cov_zero`, `cov_write`) returning the new
`CoverageError`; `TestCtx::{coverage, zero_coverage, write_coverage}` reach the running
simulation of a `#[simjig::test]`. `ModelBackend` gains defaulted `zero_coverage`,
`write_coverage` (and `can_trace`, item 6); `Models::backend_mut`, `Models::can_trace`
(kernel files, additive). A poisoned model refuses (no call reaches it).

Finding: `coverage.dat` has fewer lines than `cov_runs` has counters (26 vs 30 for
`counter_cov`); the extra counters are zero. Verilator writes one line per point name, so
the test compares the non-zero counts as multisets and checks `points <= counters`.
Tests: `e2e_stage3 coverage::counters_can_be_zeroed_and_written_with_their_names`,
`coverage::coverage_errors_name_the_model_and_the_fix`.

## 6. Nits
- `ForkError`: `Debug` is `Display` (`fork::tests::a_fork_error_debugs_as_its_message`).
- `cargo simjig waves`: `--window` parsed in the CLI (cargo-simjig cannot depend on
  simjig, so the grammar is restated there; decimals normalised to whole numbers of a
  smaller unit, which is what the harness accepts), `--help`, the usage text's first line
  lost its indent to a `\` continuation (now `concat!`), `-p` documented, nextest's exit
  100 prints a note instead of an error and the exit code is nextest's.
- No waves command when no model can trace (`FailureReport::can_trace`, from
  `ModelBackend::can_trace`); `harness_tests runner::a_test_whose_models_cannot_trace_offers_no_waves_command`.
- `SimTime` documents its print route (no `Display`: ticks need a precision); new
  `TestCtx::precision`. `WavesFile` was not exported; now it is.
- README, quickstart (`.trace_fst(true)` on the flop, `tests/inv_flop.rs`), crate docs.

## Not verified / open
- Linux. GTKWave opening the per-file `.gtkw`s.
- The generated `Dpi` trait docs (simjig-build codegen, not mine) still say only "cannot
  read or write signals or await"; they could add "or drive another simjig model".
- `docs/notes/waves.md` ("Files in the run dir") describes the old single-file replay.
- `fork_checkpoints cost::copy_on_write_shows_in_the_branch_cost` failed once under load
  (page-fault count) and passed on rerun; unrelated to these changes.
