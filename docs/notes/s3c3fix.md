# s3c3fix: Stage 3 review cycle 3 fixes (branch `fix/s3c3fix`, from `stage/3`)

One commit per fix. Each test named here failed (or did not compile) before its fix.

## 1. A branch killed by a signal pointed the waves report at a 0-byte FST (RISK)
`fork::FailedBranch` now holds `BranchEnd::{Failed(time), Died(ChildExit)}` instead of an
`Option<SimTime>`. In `harness::replay::waves_file`, a died branch's files get no marker
(`WavesFile::marker: Option<SimTime>`) and `died: Some(exit)`; the label is
`died (killed by signal 6); file incomplete` (`ChildExit`'s new `Display`), and the sort
puts them after every readable file. A `.gtkw` (and so a `gtkwave` command) is written
only for a file that starts with a whole FST header block (`has_fst_header`); with a
header but no marker, the save file's marker is `-1`.
- e2e: `waves_tests on_fail::a_fork_branch_killed_by_a_signal_is_listed_last_as_incomplete`
  (`std::process::abort()` in branch 1, under `SIMJIG_WAVES_ON_FAIL=1`). Observed: the dead
  branch's file is 0 bytes, so no `.gtkw`. Before the fix the first waves line was the
  0-byte file, "failed at t=1 us", with a gtkwave command.
- snapshot `harness_tests reports::the_file_of_a_dead_fork_branch_is_labelled_incomplete`;
  units `replay::tests::only_a_file_starting_with_a_whole_header_block_has_a_header`,
  `waves::tests::a_save_file_without_a_failure_has_no_marker`.

## 2. Nested checkpoints were refused as `OtherTasks [main.fork_sentinel]` (RISK)
New `ForkError::Nested` ("checkpoints cannot be nested inside a fork branch"), checked
first; "in a branch" is "this process holds the socket to its parent" (`imp::in_branch`,
the existing `CHANNEL`), so no new state. Supporting nesting is not trivial (the sentinel,
the channel, `ARMED`, the failed-branch record and the waves names would all need a
level), so it is refused and documented under the module's Limits and in the README.
Removed: the `ARMED` clear at each fork (the parent never arms a hook), the multi-level
`.branch-<i>.branch-<j>` parsing in `trace_identity`, and `WavesFile::branch` is
`Option<usize>` instead of a path. Test: `fork_checkpoints
rules::a_checkpoint_inside_a_branch_is_refused_as_nested`.

## 3. Nit: `at_child_exit` doc says "unless a signal kills the child".

## 4. Docs and messages
- README: "Coming from cocotb" after Quick start (table of equivalents; drop cancels,
  `detach`; deposits; pre-edge at `rising()`, settled at `ReadOnly`, `WriteInReadOnly`;
  Timer ties; the 10 ms budget, `DELTA_MAX`, `READ_WRITE_MAX`; `set` types; `Elapsed`;
  the two "Known differences" of `tests/conformance/README.md`). Quick start: `cargo
  xtask setup`, installing `cargo-simjig`, `cargo simjig doctor`, `~/.cargo/bin`.
- quickstart README "Using it outside this repository". Verified by copying
  `examples/quickstart` to the scratchpad with path dependencies and the copied
  `rust-toolchain.toml`: `pixi run --manifest-path <simjig>/pixi.toml cargo nextest run`
  built and passed its 3 tests, and a failing test's `repro:` line already carries
  `pixi run --manifest-path <simjig>/pixi.toml` (the harness's launcher detection).
  Not verified: `cargo simjig waves` from such a copy.
- `cargo simjig waves --help`: the FAIL line's name, the function name or `module::test`,
  matched exactly (the filter stays `test(=..)`).
- `Setup` messages no longer repeat their help (DPI's three, and `DutModel` without
  `load`); DPI failures name the DUT via `ModelMeta::dut_name`. This needed
  `ModelDpi::new(prefix, dut)`, so `model/verilator.rs` changed by one line (not my file).
  The slot learns the DUT's name when a model claims it (`Slot::dut`). Tests: the two
  DPI `Setup` snapshots, `dpi::tests::*`, `multi_dut fakes::a_dut_without_load_...`.
- `Sim::coverage`/`zero_coverage`/`write_coverage`: "model index, 0 = first DUT".
- `simjig::now()` says how to print it (`now().display(ctx.precision())`); this is in
  `kernel/trigger.rs`, docs only, not my file. `Display for SimTime` is not possible
  cleanly: a `SimTime` is ticks without a precision.
- "Dropped task never ran" in summary.json: not done. The kernel prints the warning and
  records `TraceKind::DroppedUnpolled`, but the recorder is a 4096-event ring, so the
  harness could only count the ones still in the window. An exact count needs a kernel
  counter (`KernelStats`), which is not mine.

## Adopted from the reviewer's patch
`e2e_stage3 coverage::each_fork_branch_counts_its_own_coverage`, `fork_checkpoints
branches::branches_leave_no_descriptor_or_process_behind`, `waves_tests
fork::a_before_fork_failure_cancels_the_rest_of_the_checkpoint`, `dpi_tests
failures::forking_inside_a_dpi_call_fails_with_the_rule`. Not adopted: the alternating
`in_child` failures (covered by the cycle 2 tests), the nested-fork probe (replaced by the
test in fix 2), and the abort inside a DPI call (the same `Died(Signaled(6))` as an abort
in the body, tested in `fork_checkpoints failures`).

## Stale elsewhere (not my files)
`ARCHITECTURE.md` "Single homes" and `docs/notes/waves.md` still describe one
`.branch-<j>` per checkpoint level; `model::trace::branch_path` appends to whatever path
the trace has, which with nesting refused is always one level.
