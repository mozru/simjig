# int-s3: building stage/3

Branch `stage/3`, from `fix/s2fix-kernel`. Merged `w3/dpi`, `w3/waves`, `stage/2` (the
director's request mid-way: s1 cycle-2 fixes, `cov_runs`, prefix scheme v2), `w3/fork`,
in that order, with the gate after each.

## Textual conflicts
- **dpi**: `crates/simjig/build.rs` takes stage/2's simjig-build model list and gains
  `dpi`/`dpi_types` (`build_dpi.rs` removed; `dpi_tests` use `simjig_test_models`, and its
  `missing_debug_implementations` expect is gone since `Dut` derives `Debug`). `dut.rs`
  runs both the DPI prototype cross-check and stage/2's duplicate-export claim. `emit.rs`
  appends `emit_dpi` to the new `emit_rust(.., &RustOptions)`.
- **waves**: `model/verilator.rs` keeps the DPI scope and the tracer.
- **stage/2**: `ARCHITECTURE.md` invariants; `codegen_it` gains `coverage` and `dpi`.
- **fork**: `model::bind::load` passes the trace options; Cargo files keep `libc` and
  `miniz_oxide`.

## Semantic issues found
1. Compile breaks from stage/2: `try_current` now takes `what` (dpi, fork); the harness's
   `human_time` was removed (waves uses `SimTime::display`); `FailureKind` gained
   `ReadWriteLimit`/`DpiPanic` (`failing_signals`); `Design` gained fields (hash tests).
2. **Trace order against DPI poisoning**: a DPI failure poisons only the Rust side; the
   waves now keep the failing timestep (dump and flush before poisoning).
3. **Multi-DUT tracing**: every model of a tuple got the same `ModelOptions`, so all
   opened the same FST path. Model `i > 0` now writes `<stem>.model-<i>.fst`.
4. **Fork children never finished their FST** (they `_exit`), so a branch file had no
   header. `ForkHook::at_child_exit` closes it. Flushing before the fork is *not* done:
   it split a value-change block and fstcpp repeated a timestamp in the parent (seen in
   the test); the detach already discards the child's copy of the buffer.
5. **Prefix claims broke on any `DutConfig` change**: the identity is a hash of its
   `Debug` form, so adding `dpi_in_c` made every cached prefix a "collision" (the
   fixture-crate tests failed on this worktree's cache). Claims are now namespaced by a
   scheme hash (version plus `DutConfig::default()` shape).
6. DPI failures other than panics (no implementation, name clash) stay `ModelFatal`.
7. The kernel evaluates only models with work in a timestep, so a model's waves hold its
   own timesteps, not the simulation's (`each_model_of_a_multi_dut_simulation_*`).

## Open
- IR `SCHEMA` was not bumped for the optional DPI fields (dpi notes item 10).
- `simjig` build-depends on `simjig-build` for its tests (dpi notes item 5).
- `Sim::coverage` names counters by index only; `coverage.dat` has the point names.
- Not verified: Linux, GTKWave opening the files, `--timing` models calling DPI.
