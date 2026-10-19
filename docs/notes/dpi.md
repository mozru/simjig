# DPI notes (agent "dpi", branch `w3/dpi`)

DPI-C imports implemented in Rust: the one sanctioned case of the simulator calling the
testbench. Checked against Verilator 5.052 (pixi), osx-arm64.

## Files
| Where | What |
|---|---|
| `svtypes::ir::{DpiImport, DpiArg, DpiType}`, `Design::{dpi_imports, dpi_exports}` | The IR (additive; omitted from JSON when empty). |
| `svtypes/src/frontend/v5_05x/dpi.rs` | Decoding the `FUNC`/`TASK` nodes with `dpiImport`/`dpiExport`. |
| `simjig-build/src/codegen/dpi.rs` | `emit_dpi` (trait, `Dut::set_dpi`, trampolines), `dpi_prototypes`, `check_dpi_header`. |
| `emit.rs` (one line), `dut.rs` (`check_dpi_header` after the port check) | Registration. |
| `simjig/src/dpi.rs` | Runtime: slots, `install`, `call`, `ModelDpi::scope`, `forbid_in_call`, `Chandle`. |
| `simjig/src/dpi/abi.rs` | The only `unsafe`: ABI markers, `InArg`/`OutArg`/`InOutArg`, `dpi_import!`. |
| `model/verilator.rs`, `signal.rs` | Minimal hooks: scope around `eval`/`final`; `forbid_in_call` in `get`/`set`/triggers. |
| `simjig/build_dpi.rs` (+3 lines in `build.rs`), `tests/dpi_tests/` | Real-pipeline tests. |

## What a design gets
For `import "DPI-C" function int dpi_add(input int x, input int y)` the generated module has
```rust
pub trait Dpi { fn dpi_add(&mut self, x: i32, y: i32) -> i32; /* ... */ }
impl Dut { pub fn set_dpi(&self, imp: impl Dpi + 'static) { /* install */ } }
::simjig::__private::dpi_import! { prefix = "Vdpi_…", imp = dyn Dpi,
    fn dpi_add => dpi_add(x: InArg ::simjig::__private::dpi::I32, …) -> …::I32 }
```
Inputs are values, `output`/`inout` are `&mut` (an output starts at zero, an inout at the
design's value), the result is the return type; a task or `void` function returns `()`.
Types: `bit`/`logic` -> `Bit`; `bit`/`logic [N-1:0]` (N <= 64) -> `Bits<N>`; `byte`,
`shortint`, `int`, `longint` -> `i8`..`i64` (`unsigned` -> `u8`..`u64`); `chandle` ->
`simjig::Chandle`. Anything else is a codegen error naming the import, its source line and
the argument (snapshot `codegen_it__dpi__unsupported_imports_are_errors_naming_the_import`).

`#[no_mangle]` and every `unsafe` block are inside `dpi_import!`, a simjig macro, so they do
not trip `unsafe_code` in the user crate: `tests/dpi_tests` is `#![forbid(unsafe_code)]`
and compiles.

## API decision: `dut.set_dpi(imp)`
Chosen over a `DutModel` hook (e.g. `WithDpi<Dut, Impl: Default>` as the test's parameter
type) because:
- it reads like the rest of the API, works the same with `#[simjig::test]` and with a
  hand-built `Sim`, and takes a value (an implementation can capture test state, an
  `Rc<RefCell<..>>` scoreboard, a seed);
- it is early enough: the main task runs before the first eval at t = 0 (kernel step 1), so
  a call at the top of the test body precedes every DPI call, including the initial
  settle of `assign sum = dpi_add(a, b)`. Before `Sim::run` works too.
- It is per instance and replaceable (takes effect from the next call).
A DPI call with no implementation fails the run saying exactly this.

How the two meet without touching the kernel: a thread-local registry keyed by
(simulation id, model prefix). `set_dpi` takes the id from one of the `Dut`'s handles
(`RawSig::sim_gen`); the backend, on its first eval inside a run, from the running kernel
(`state::try_current`). Whoever comes first creates the slot. The backend then caches it.

## Runtime behaviour
- **Dispatch.** `VerilatorModel::eval` (and `final`) runs inside `ModelDpi::scope`, which
  makes the instance's slot current on the thread; `call` takes it, downcasts the
  implementation to the generated trait, runs it under `catch_unwind`, puts it back.
  Isolation follows: two instances in sequence, or on two threads at once, never cross
  (`basics::two_instances_*`, `basics::instances_on_two_threads_*`, 2000 interleaved calls).
- **Never unwinds into C++.** A panic, a missing implementation, or a call that reached
  another model's trampoline is recorded in the slot; the trampoline writes every output
  (from zero; Verilator's output buffers are uninitialised) and returns zero. Later calls
  in the same eval return zero without running the implementation. A panicking panic
  payload `Drop` is caught too. A call with no current model at all (C code outside an
  eval, a nested DPI call) prints to stderr and returns zero: there is nothing to fail.
- **After the eval** the backend takes the failure, poisons the model (no further eval, no
  `final`), and returns it as the eval's `ModelError`; the kernel reports
  `ModelFatal { message }`: ``DPI-C import `dpi_add` of model `Vdpi_b6b4d2f2` panicked:
  seven is not allowed. The design received zero as its result, so the model is poisoned
  and is not evaluated again``. Under `#[simjig::test]` that is the failure report
  (`failures::a_panic_in_a_harness_test_is_the_failure_report`).
- **Signals and awaiting inside a call.** Typed `get`, `set`, `changed`, `rising`,
  `falling` panic with ``DPI-C import `dpi_tick` read a signal: DPI functions run inside
  eval; they can't access signals or await`` (so the failure carries the rule).
- **Duplicate names.** A trampoline knows the prefix it was generated for; a call from a
  model with another prefix (two models importing one C name, one trampoline linked) is a
  failure naming both. `set_dpi` also claims its C names process-wide and panics if another
  prefix claimed one. Two instances with the *same* prefix in one simulation (multi-model
  sims) cannot be addressed separately by `set_dpi`; the second one fails on its first
  DPI call, not before (models without DPI are unaffected).

## Facts about Verilator 5.052 (beyond spikes §3)
- `--json-only` dumps `dpiImport`, `dpiExport`, `cname`, `prototype`, but not `dpiTask`,
  `dpiContext` or `pure` (`AstNodeFTask::dumpJson`). A `void` function is a `TASK` node.
  So a task and a `void` function are indistinguishable; Verilator declares the task's C
  function `int` (ignored at the call site, `t_task(x__Vcvt);`) and the function's `void`.
  The trampoline returns `int` 0 for both; returning an `int` to a caller expecting `void`
  only leaves a value in the return register. The header check accepts either.
- Every vector argument is a pointer, even 8 bits (`const svBitVecVal*`); a vector result
  is `svBitVecVal` by value, and Verilator rejects results above 32 bits and 4-state
  vector results. `byte unsigned`, `int unsigned`, ... are the `unsigned` C types.
- `import "DPI-C" c = function f` gives `cname` = `c`; the C symbol is `c`.
- A parameterised module cloned twice lists its import twice; the IR keeps one per C name
  and errors if two declarations of one C name disagree.

## Cross-check against `V<prefix>__Dpi.h`
`dpi_prototypes` renders each trampoline's C prototype from the IR; `check_dpi_header`
compares types (not names) with the header's `extern` lines after verilation (`dut.rs`,
`Error::Parse` naming the header line). Tested on both designs against the pinned
Verilator, plus a drifted and a missing prototype (spike rule 4: "the signatures cannot
drift"). Imports with an unsupported type are skipped (no trampoline exists for them).

## Measured (release, Apple M6; single runs, machine shared with other builds)
| What | ns |
|---|---|
| Trampoline call, Rust caller through `extern "C"` (`dpi::abi::tests::measure_trampoline`), quiet machine | 3.1 - 3.6 (plain `extern "C"` call 0.4) |
| Same, load average 27 to 67 | 4.6 - 26 |
| `ModelDpi::scope` around one eval, load average 27 to 67 | best 6.1, up to 41 (not measured on a quiet machine) |
| End to end (`dpi_tests::bench`): posedge timestep calling `dpi_tick` minus negedge timestep | 11 - 13 (one call plus a flop update) |

The first version (two thread-locals, `RefCell` for the failure flag, string compare of
the prefix) cost 7.0 - 7.3 ns per call; one thread-local, a `Cell<bool>` and a pointer test
of the prefix brought it to ~3.2 ns. The spike's 1.5 ns had no per-instance dispatch
(no downcast, no dynamic call, no prefix check). Every eval of every model now pays the
scope (two thread-local accesses, an `Rc` clone); `ModelMeta` carrying the import list
would let models without DPI skip it (below).

## Why not simpler (constitution rule 5)
- **A registry instead of a field on `Dut` or `ModelMeta`:** both are other agents' code
  (Rust item emitter, `dut_model!`); the registry needs neither, and the kernel is untouched.
- **`unsafe` in `dpi::abi`:** the trampolines must read and write the pointers Verilator
  passes. Each `unsafe fn` has one contract (the pointer is Verilator's argument of exactly
  that type); the macro is the only caller. Unit tests drive every shape with buffers.
- **Markers plus `Param` wrappers instead of per-type code in the generated file:** the
  generated code stays free of `unsafe`, and one table (`abi.rs`) defines the ABI.
- **`Box<Box<dyn Trait>>` behind `dyn Any`:** the runtime does not know the generated trait.

## Open issues and hooks for other agents
1. **`FailureKind::DpiPanic`** (kernel): today the failure is `ModelFatal` with the DPI
   message. A dedicated kind needs `ModelError` to carry a kind; `dpi::ModelDpi::scope`
   already returns the message separately, so `VerilatorModel::eval` is the only caller to
   change.
2. **Kernel-wide `forbid_in_call`** (kernel): only typed handles check it. A `RawSig` read
   inside a call panics with `RefCell` "already mutably borrowed" (the model is borrowed
   during eval), which still becomes a DPI failure but with that message; `spawn`, `Timer`
   or `now()` inside a call are not caught. One call to `crate::dpi::forbid_in_call(what)`
   in `state::with_current`/`try_current` covers everything.
3. **Panic location** (kernel2 `panics.rs`): `dpi::dispatch` could call `panics::take()`
   after `catch_unwind` to add `at file:line` to the message, as task panics do.
4. **build.rs** (glue): `build_dpi.rs` is self-contained; when glue's simjig-build-based
   `build.rs` lands, fold its two `Dut`s into that list and delete `build_dpi.rs`
   (`tests_only` duplicates glue's; the `dut_model!` append skips itself when codegen
   emits the glue). `tests/dpi_tests/main.rs` expects `missing_debug_implementations`
   until `Dut` derives `Debug`; remove that `expect` then.
5. **simjig build-depends on simjig-build** for its own tests, contradicting the
   ARCHITECTURE invariant "build-time and run-time crates never depend on each other" (as
   glue's change does). Needs a decision.
6. **C implementations:** codegen always emits trampolines, so a user who links C DPI code
   (like `tests/duts/dpi/dpi_impl.c`) gets duplicate symbols. Needs a `Dut` builder switch
   (e.g. `.dpi(DpiImpl::C)`), not added (not my file).
7. **Exports** are in the IR (`dpi_exports`) for the build-script duplicate check; the
   runtime registry covers imports only (exports are Verilator's C++ symbols).
8. **`ModelMeta::dpi_imports`** would let the backend skip the per-eval scope for models
   without DPI and register names at load (not only in `set_dpi`).
9. No CHANGELOG exists in the repository; the generated-code change is covered by the new
   snapshots (`codegen_it__dpi__*`, `codegen_it__snapshots__dpi__*`).
10. IR `SCHEMA` was not bumped: the new fields are skipped when empty, so the JSON (and
    every `IR_HASH`) of designs without DPI is unchanged; designs with DPI did not decode
    differently before (their imports were ignored). Bump it if the policy is strict.

## Not verified
- Linux (x86-64 and aarch64; C `char` is unsigned on aarch64 Linux, `i8` is still the
  right ABI type for a one-byte argument). `--timing` models calling DPI from coroutines.
- `svGetScope()` and other `svdpi.h` calls from a Rust `context` implementation (there is
  no binding). DPI exports called from Rust. Open arrays, structs, `string`, `real`.
- The per-eval scope cost on a quiet machine.
