# Changelog

All notable changes to simjig. Constitution rule 4 requires an entry for every change to
the public API, the generated code or simulation semantics.

## Unreleased
- Initial workspace:
  - `svtypes`: the IR from Verilator `--json-only`;
  - `simjig-bits`: bit-vector values;
  - `simjig-build`: the pipeline, codegen and C ABI runtime;
  - `simjig`: kernel, model, typed handles, harness;
  - `simjig-macros`: `#[simjig::test]`;
  - `cargo-simjig`: `doctor`.
- C ABI version 1, vtable size 192, with trace, coverage and RNG slots.
- IR schema 2: adds `unrepresentable` constants.

### Stage 3: DPI-C, waves, fork checkpoints, several DUTs per test

#### DPI-C imports implemented in Rust (`simjig`, `simjig-build`, `svtypes`)
- Generated code: for a design with `import "DPI-C"` functions or tasks, the generated
  module has a `Dpi` trait (`DutDpi` if the design names a type `Dpi`), one method per
  import, `Dut::set_dpi(impl Dpi)`, and one `extern "C"` trampoline per import (through
  `simjig::__private::dpi_import!`, so a crate with `forbid(unsafe_code)` compiles it).
  Snapshots: `codegen_it__dpi__*`, `codegen_it__snapshots__dpi__*`.
- New `simjig::Chandle` (an SV `chandle`).
- `dut_model!` takes an optional `dpi_imports = [..]`; `ModelMeta` gains `dpi_imports`
  (the C names of the Rust-implemented imports). Models without any skip the per-eval
  DPI scope.
- A panicking implementation fails the run as `FailureKind::DpiPanic`, with the panicking
  line in `Failure::location` and in the message; a call with no implementation, or a
  clash of import names, is `FailureKind::Setup` with what to change. Either poisons the
  model.
- **Limit:** two instances of one DPI build cannot run in one simulation (lockstep
  included): `set_dpi` cannot address the second one, whose first DPI call fails the run
  as `Setup`. Build the design a second time under another `simjig_build::Dut::new` name.
- Inside a DPI call every kernel entry point (signal reads and writes, triggers, `spawn`,
  `now()`, timers, sync primitives) panics with "DPI-C import `f` used <what>: DPI
  functions run inside eval; they can't access signals or await".
- `simjig_build::Dut::dpi_in_c(c_names)` (`DutConfig::dpi_in_c`) leaves those imports to
  C or C++ (for example in `CppRecipe::model_sources`): no trait method or trampoline is
  generated for them, so a C definition links without a duplicate symbol. An unknown name
  is an `Error::Config`. `DutConfig::rust_dpi_imports(&design)` lists the others.
- `simjig_build::codegen::{emit_dpi, dpi_prototypes, check_dpi_header,
  DpiHeaderMismatch}`; `emit_dpi` takes the import list. `RustOptions` gains
  `dpi_imports`. The build cross-checks the trampolines' prototypes against
  `V<prefix>__Dpi.h`.
- `svtypes::ir::{DpiImport, DpiArg, DpiType}` and `Design::{dpi_imports, dpi_exports}`.
  Both are left out of the JSON when empty, so designs without DPI serialise as before;
  the IR schema stays 2.

#### FST waves (`simjig`, `cargo-simjig`)
- New `simjig::TraceOptions` (`new`, `depth`, `start`, `end`, `if_supported`,
  `with_path`, accessors) and `ModelOptions::{trace, with_trace}`. A traced model writes
  one sample per timestep it evaluated, at the end of the timestep.
- `LoadError::{NoTrace, TraceOpen}`.
- Waves on failure: `SIMJIG_WAVES=1`, `SIMJIG_WAVES=window:<t0>..<t1>` and
  `SIMJIG_WAVES_ON_FAIL=1` replay the test in a child process with tracing and write
  `waves.fst`, `waves.gtkw` and `replay/` to the run directory. `SIMJIG_TRACE`,
  `SIMJIG_TRACE_FROM` and `SIMJIG_TRACE_TO` trace a run directly.
- `FailureReport` gains `signals` and `waves` (`simjig::{WavesReplay, Verdict}`); the
  report's `waves:` line is now the working command `SIMJIG_WAVES=1 <repro>` (report
  snapshots changed in that line only). `TestOutcome` gains `precision`, `RunEnv` gains
  `waves` and `trace`. When no model of the test can trace, the line says so instead
  (`FailureReport::can_trace`).
- A replay keeps every trace file it wrote: one per model (`waves.fst`,
  `waves.model-<i>.fst`) and per fork branch (`waves[.model-<i>].branch-<j>.fst`), each
  with its own `.gtkw`. `WavesReplay::files: Vec<WavesFile>` (model, branch path, marker,
  save file; new `simjig::WavesFile`) replaces `WavesReplay::{fst, gtkw}`. When a fork branch fails the test, its
  files come first in the report, with the marker at the branch's own failure.
- `cargo simjig waves <test> --seed <seed> [--window ..]`. It checks `--window` itself
  (decimals such as `1.5us` become `1500ns`; a bad window is a usage error naming the
  forms), has `--help`, and after a failing test prints a note, not an error, keeping
  nextest's exit code.
- A traced model in a `fork` branch child continues its waves in
  `<stem>.branch-<i>.fst`; a child never writes the parent's file.
- In a simulation of several models, model `i > 0` traces into `<stem>.model-<i>.fst`
  (new `ModelOptions::for_model`); model 0 keeps the given path.

#### Failure kinds (`simjig`)
- New `FailureKind::Setup { message, help }`: the test's setup cannot run and the design
  is not at fault, with advice that says what to change. It replaces `ModelFatal` (whose
  help blamed the design) for a DPI-C call with no implementation or that cannot reach
  its instance, for every `LoadError` but `Create` (a stale build, `NoTrace`,
  `TraceOpen`, an unsupported precision), for DUTs that cannot share a simulation, and
  for a DUT type without `DutModel::{load, bind}` in a multi-DUT test.
- New `LoadError::help() -> Option<&'static str>`.

#### Several DUTs per test and lockstep (`simjig`, `simjig-macros`)
- `#[simjig::test]` accepts 2 to 4 DUT parameters, which share one simulation in
  parameter order. Five or more is a compile error.
- `DutModel` gains defaulted `load` and `bind`; new `simjig::LoadedModel`; `DutModel` is
  implemented for tuples of 2 to 4 DUTs. `dut_model!` emits `load` and `bind`.
- New `simjig::{Lockstep, Mirror, Divergence}`.

#### `fork()` checkpoints (new module `simjig::fork`)
- `run_from_checkpoint(seed, n, body)`, `Branch`, `Branches`, `BranchOutcome`,
  `BranchFailure`, `BranchError`, `ChildExit`, `BranchCost`, `RemoteFailure`,
  `BranchValue`, `ForkError`, `ForkHook` (with `at_child_exit`), `register_hook`,
  `HookGuard`. Unix only, with at most two threads in the process (nextest).
- `ForkError`'s `Debug` is its `Display`, so `.unwrap()` prints the advice.

#### Coverage counters
- New `Sim::coverage(model) -> Option<Vec<u32>>`: the counters of a model built with
  coverage, in the order of the ABI's `cov_runs`.
- New `Sim::zero_coverage(model)` (the ABI's `cov_zero`) and
  `Sim::write_coverage(model, path)` (`cov_write`: Verilator's `coverage.dat`, which names
  every point), both returning `Result<(), CoverageError>` (new `simjig::CoverageError`).
- New `TestCtx::{coverage, zero_coverage, write_coverage}`: the same, on the running
  simulation of a `#[simjig::test]`.

#### Build (`simjig-build`)
- An `-rpath` link argument is emitted once per build script, so FST models no longer
  make ld64 warn `duplicate -rpath`.
- Prefix claims in the cache moved to `prefixes/<scheme>/<prefix>`, the scheme naming
  the simjig-build version and the shape of `DutConfig`; a cache written by another
  simjig-build no longer reports every prefix as a collision.
