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
- Scheduler: `Timer`s due at the same time fire one at a time, in registration order,
  each followed by every task it makes ready (and every task those wake), as cocotb runs
  its event loop after each Timer callback. Before, all of them woke together and tasks
  they woke ran after the last Timer task.
- Scheduler: value changes fire one (signal, edge kind) group at a time, oldest-primed
  first, each followed by every task it makes ready, as cocotb runs one VPI value-change
  callback at a time with its event loop inside each (README "Callback order"; goldens
  `coincident_join`, `changed_join`, `written_join`, `edge_kinds`). A task woken by one
  change that awaits a signal whose group another task already waits on, and which has
  not fired yet, now resumes in the same delta (it used to wait for the next change), and
  tasks those wakes make ready run before the next group's waiters. `rising`, `falling`
  and `changed` on one signal are separate groups. Wake order at one delta changes from
  global registration order to priming order; trace hashes of runs where that differs
  change.
- `first!`, `join!` and `with_timeout` resume their caller where cocotb 2.1's `First`,
  `Combine` and `with_timeout` do (golden `combinator_hop`): after yielding once, and
  once more when dropping the losers cancelled a wait that had not fired, so the tasks
  woken by the same edge (and, for `first!` and `with_timeout`, the tasks those wake) run
  first. Before, the caller resumed at once, in its registration order. A combinator
  whose operands are ready at once now also yields before returning.
- `with_timeout` breaks ties as cocotb 2.1 does (golden `timeout_ties`): the timeout's
  Timer registers before the future's triggers and is checked first, so a `Timer`,
  edge or `cycles` due exactly at the timeout now times out (a future's own `Timer` used
  to win); a spawned task whose `Timer` was registered before the call still returns its
  result.
- `BlockedTask` implements `Display` (`` `main.d` awaits rising(clk) ``), the rendering
  `Failure`'s `Display` and the failure report share.
- `#[simjig::test]` has a default simulation-time budget of 10 ms (`DEFAULT_TIMEOUT_MS`),
  rounded up to a whole tick (`ceil(10 ms / tick)`, at least one) at precisions coarser
  than 1 ms; `timeout = "none"` disables it. Before, a test without `timeout` had no
  limit.
- `TestCtx::rng(path)` panics, at the second call, when `path` was already taken in the
  run (the message names the first call's location): two streams with one path would
  silently repeat each other's values.
- `FailureKind::TimeLimit` gains `blocked`: the tasks still waiting, and on what, as for
  `Starved` and `Deadlock`.
- New `FailureKind::ReadWriteLimit { limit, waiters }`: more than `READ_WRITE_MAX`
  (10 000) `ReadWrite` rounds that woke a task in one timestep.
- New `FailureKind::DpiPanic { message }`: a DPI-C import implemented in Rust panicked
  during an eval; the model is poisoned.
- Times print in the largest unit that shows them exactly (`1250 ns`, `1 us`; zero in
  the precision's unit, `0 ps`), through `SimTime::display` (`TimeDisplay`), in failures,
  reports and `summary.json`.
- The repro line is `SIMJIG_SEED=<seed> [<launcher> ]cargo nextest run -p <package>
  [--run-ignored all] -E 'binary(=<binary>) & test(=<test>)'`, with the launcher
  (`pixi run [--manifest-path ..] [-e <env>]`, `conda run -p <prefix>`) the test ran under.
- Run directories are `<root>/<package>/<binary>/<test>/<seed>/` (was
  `<root>/<test>/<seed>/`), written to a temporary sibling and renamed into place.

- IR schema 3: `Module::instances` (`Instance { name, module }`) and `Module::variables`
  (`Variable { name, kind: VariableKind::{Signal, Parameter, Localparam} }`), both omitted
  from the JSON when empty. Every IR hash, and so every generated `IR_HASH`/`kIrHash`,
  changes once.
- `codegen::lower` checks each probe path against the IR's instance tree: a mistyped
  instance or variable (with the closest name), a declaring module that is not the
  instance's, and a `ProbeAccess::ReadWrite` probe of a parameter or localparam are
  `codegen::Error::Config` errors, so the build fails before verilating.
- C ABI runtime: `create` refuses a probe bound for writing when its variable is a
  parameter (constant storage) or only `public_flat_rd` (`EXCEPTION`, model poisoned).
- After verilating, the build checks each probe the shim binds against Verilator's scope
  table (`V<prefix>__Syms__Slow.cpp`): a wrong width, an unpacked variable, a missing
  public scope or variable, or a writable probe of a constant fails the build
  (`Error::Config`) instead of the first `create`. `Emitted` has a new field `probes:
  Vec<ProbeSite>` (the probes the shim binds; `codegen_emitter` fills it, `Emitted::default()`
  leaves it empty), a breaking change for emitters that build `Emitted` without
  `..Emitted::default()`.
- C ABI: `cov_runs` is documented to cover code coverage counters only; covergroup bins
  (allocated when `new` runs) are in no run, while `cov_write` and `cov_zero` include them.
  The runtime stops noting coverage counters once create has built the runs.
- The build no longer emits `cargo::rerun-if-changed` for the generated inputs it stores in
  the cache (`inputs/<hash>/`: the `.vlt`, the interface wrapper, `verilator_input`s), so
  the first no-op build after a new DUT configuration no longer reruns the build script.
- A DUT whose interface ports are not all bound fails with one line per unbound port and
  the `.bind_iface(..)` call that binds it (`Error::Config`), instead of Verilator's error
  dump.
- `svtypes::error::Error::Tool::stderr` now holds the tool's whole stderr (it held the last
  40 lines); `Display` still shows the last 40.
- Compiled model archives are cached in the shared cache (`<cache>/archives/<key>/`,
  `libsimjig_model_<prefix>_<key16>.a`) instead of being compiled into `OUT_DIR` on every
  build-script run: editing one DUT recompiles only that DUT's C++. Every archive key
  (models and runtime) now also covers the files under `CppRecipe::include_dirs`.
- `svtypes` front-end errors with a source location print it first:
  `<file>:<line>:<col>: <message> (Verilator <version> JSON at <path>)`. A type error
  reached from a port names the port and its declaration, and an unpacked struct or union
  port says to declare the type `packed`.
- `Error::Command` leads with what the run was for (new field `what`, e.g. ``JSON pass for
  DUT `shreg` ``, ``verilating DUT `x` ``) and ends with the command line; its `status` is
  now the exit code (`Option<i32>`, `None` for a signal) instead of a string.
- `Dut::verilator_args` refuses the arguments the pipeline sets itself (`--prefix`,
  `-Mdir`/`--Mdir`, `--top-module`/`--top`, `--cc`, `--exe`, `--build`), each with the
  builder method or setting to use instead.
- `cargo simjig doctor` and the build's Verilator errors say "install Verilator 5.052 (in
  the simjig repo: `pixi install --locked`)" (and the same for the other tools) instead of
  assuming the simjig repository; the doctor's Verilator line says that builds keep the
  Verilator their build script found, so switching `PATH` alone does not rebuild.
- `svtypes --help` (`-h`, `help`) prints the usage and succeeds.
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
- The generated `Dpi` trait's docs say that an implementation can't drive another simjig
  model inside a call.
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
  they do not change the IR schema version.

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
  save file; new `simjig::WavesFile`) replaces `WavesReplay::{fst, gtkw}`. When a fork
  branch fails the test, its files come first in the report, with the marker at the
  branch's own failure.
- `cargo simjig waves <test> --seed <seed> [--window ..]`. It checks `--window` itself
  (decimals such as `1.5us` become `1500ns`; a bad window is a usage error naming the
  forms), has `--help`, and after a failing test prints a note, not an error, keeping
  nextest's exit code.
- A traced model in a `fork` branch child continues its waves in
  `<stem>.branch-<i>.fst`; a child never writes the parent's file. The branch's file is
  finished (header written) also when the branch poisons the model: after a DPI failure
  it is closed normally, after a `$fatal` by destroying the model in the child.
- In a simulation of several models, model `i > 0` traces into `<stem>.model-<i>.fst`
  (new `ModelOptions::for_model`); model 0 keeps the given path.

#### Test context and time (`simjig`)
- New `TestCtx::precision()`, to print a time in a test body
  (`now().display(ctx.precision())`); `SimTime` documents why it has no `Display`.

#### Documentation
- `README.md`, `examples/quickstart` and the crate docs show the Stage 3 features. The
  quickstart's flop is built with `.trace_fst(true)` and gains a two-DUT test
  (`tests/inv_flop.rs`).

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
