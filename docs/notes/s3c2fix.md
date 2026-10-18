# s3c2fix: Stage 3 review cycle 2 fixes (branch `fix/s3c2fix`, from `stage/3`)

One commit per fix; each test named here was run and seen failing before its fix.

## 1. A failing `in_child` hook let the child corrupt the parent's FST (BUG)
Three independent defects, each fixed and tested on its own:
- `fork::finish_child` ran every hook's `at_child_exit`. Now each child records the hooks
  whose `in_child` succeeded (`ARMED`, cleared at each fork, so a nested checkpoint's
  child does not inherit its parent branch's) and finishes only those. Tests:
  `waves_tests fork::a_hook_failing_before_the_trace_detaches_leaves_the_parents_fst_alone`,
  `fork::an_uncreatable_branch_file_fails_the_branch_and_leaves_the_parents_fst_alone`,
  `fork::an_uncreatable_branch_file_of_model_0_leaves_model_1s_parent_fst_alone`,
  `fork::an_uncreatable_branch_file_of_model_1_leaves_its_parent_fst_alone` (the parent
  files are read with the FST block-chain reader and must hold exactly the parent's
  timesteps; before the fix the chain ran past the end of the file); unit
  `fork::tests::a_child_finishes_only_the_hooks_whose_child_half_succeeded`.
- `TraceState::reopen` kept `file_open` after a skipped (trace already stopped) or failed
  reopen; it now forgets the file (`forget_inherited`). Tests:
  `model::verilator::tests::a_fork_child_never_finishes_a_trace_it_did_not_reopen` (a
  DPI-stopped trace made the child call `trace_close`; a FATAL-poisoned one made it
  `destroy`), `a_failed_reopen_leaves_no_file_to_close`. The fake model's traced vtable
  records `trace_reopen`; `fork::run_child_hooks_in_place` (test only) runs a child's hook
  sequence without forking.
- C `trace_reopen` returned `IO_ERROR` before redirecting. It now redirects and closes
  first, then checks that the path is not the inherited file, then opens (whose
  `probe_writable` reports an uncreatable path). If the descriptor cannot be found or
  redirected, the writer is `release()`d unclosed: its destructor would write the header
  into the parent's file, and a fork child loses only its own copy. C++ test:
  `fork_child_with_an_uncreatable_path_leaves_the_parent_alone` (parent bytes unchanged
  after the child's reopen, close and destroy). `fork_child_detaches_and_reopens` still
  passes: its same-file reopen now detaches, and the retry opens afresh (depth and start
  of a no-trace reopen are 0, as documented).
- Not covered by a test: the "cannot redirect" branch (needs `/dev/null` to fail to open).
- With fix 1a alone, every e2e reproducer passes; 1b and 1c are defence in depth, each
  proven by its own unit or C++ test.

## 2. `cargo simjig waves` was not installable from the README (RISK)
README and the quickstart README give `pixi run cargo install --locked --path
crates/cargo-simjig`. Verified from a fresh `git clone` of this branch (at a short path:
pixi refuses a prefix longer than its placeholder): the install took 5 s; without it
`cargo simjig` is "no such command"; with it, `pixi run cargo simjig waves q_follows_d -p
quickstart --window 10ns..` from `examples/quickstart` wrote `waves.fst` and reported
"replay: reproduced". A cargo alias was not used: `.cargo/config.toml` is not mine, and an
alias does not pass `simjig` as argv[1] the way cargo runs a subcommand binary.

## 3. Nits
- Load failures: `ModelMeta::module` (from `module_path!()` in `dut_model!`) names the DUT;
  `LoadError` `Display` is the problem only, `help()` the advice (snapshot
  `a_model_built_without_fst_says_how_to_enable_it` updated and reviewed). Not changed:
  the DPI `Setup` messages in `simjig::dpi` (not my files) also repeat their help and name
  the prefix.
- `SIMJIG_WAVES_ON_FAIL` skips the replay when no model can trace. Trade-off: this drops
  the determinism check for such runs under `ON_FAIL` (DECISION §1: the replay doubles as
  it); `SIMJIG_WAVES=1` still replays and checks. Test:
  `on_fail::waves_on_fail_does_not_replay_a_run_that_cannot_trace`; the non-determinism
  test now uses `SIMJIG_WAVES=1`.
- Fork branch report header: `on_fail::a_failing_fork_branch_is_reported_at_its_own_time`.
  The failed branch is taken once into `FailureReport::failed_branch` (it used to be
  taken only when a waves replay ran). `summary.json`'s failure time stays the
  checkpoint's.
- Lockstep: both signals named. `counter_rev2`'s top module is `counter`, so both names
  are `counter.count`; the DUT name is not reachable from a signal handle without a
  kernel change (`ModelBackend` is not mine), so equal names are told apart by model
  index.
- STMTDLY/NOTIMING hint: `simjig-build` `error::tests::timing_diagnostics_suggest_timing_true`.
  Verilator 5.052's messages were checked by hand (`#5` gives `%Warning-STMTDLY`, an
  `@(negedge)` inside a procedure `%Error-NOTIMING`); not checked through a real failing
  `build.rs`.
- Coverage docs: `TestCtx` and the README say "model index, 0 for the first DUT".
  `Sim::coverage` (in `kernel::sim`, not mine) still says only "model `model`".
- ARCHITECTURE "Single homes": trace file names, and the window grammar's two copies.
- `Vtable::leak` documents the intended leak and `-Zmiri-ignore-leaks`.
- C++ test `destroying_a_poisoned_handle_closes_its_trace`.
