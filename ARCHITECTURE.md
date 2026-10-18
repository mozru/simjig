# Architecture

Decision record: `docs/architecture/DECISION.md`.

**Scope:** making Verilator and Rust work together well (types, build, model access,
scheduling, time, errors, waves). Verification methodology and bus models (VIP, AXI,
scoreboards, constrained random, coverage closure, RAL) are out of scope; they belong to
projects built on top of simjig.

## Code map
| Crate | Owns |
|---|---|
| `svtypes` | Versioned type IR built from Verilator `--json-only`: widths, offsets, enums, params, ports, interfaces. Python/cocotb emitter. Its `version` module is the single table of supported Verilator versions (`VerilatorVersion`, `SUPPORTED`) for every build-time crate, and its `toolchain` module the single rule for which Verilator to run and whether to accept it (`locate`, `judge`), used by `simjig-build` and `cargo simjig doctor`. |
| `simjig-bits` | Bit-vector values with SystemVerilog semantics (`Bits<N>`, `WideBits`, `Packed`). |
| `simjig-build` | Build time: one lowering feeding every emitter (Rust, C++ vtable, `.vlt`, wrapper SV, layout check, DPI-C trait and trampolines in `codegen::dpi`, with `Dut::dpi_in_c` leaving chosen imports to C), Verilator invocation, cache, per-build-script checks (`script`: DUT names, DPI exports, covergroups, one `-rpath` per directory). |
| `simjig` | Runtime: `kernel` (scheduler, triggers, tasks, failures, recorder; refuses every entry point inside a DPI-C call), `model` (Verilator backend over the C ABI; `model::trace`: FST waves sampled per timestep and moved to a per-branch file in a fork child; coverage counters via `Sim::coverage`), `signal` (typed handles), `dpi` (DPI-C imports implemented in Rust, dispatched per model instance; a panic is `FailureKind::DpiPanic`), `harness` (`#[simjig::test]` support, seeds, reports, run dirs; `harness::waves`/`replay`: waves on failure from a traced replay in a child process; `harness::multi`: several DUTs per test as one `DutModel` tuple, `Lockstep`, `Mirror`), `fork` (`run_from_checkpoint`: branches forked from a quiet point, results over a socket, `ForkHook`s for resources a child must not share). |
| `simjig-macros` | `#[simjig::test]` attribute (thin; logic unit-tested). |
| `cargo-simjig` | User CLI: `cargo simjig doctor` (environment checks with fixes) and `cargo simjig waves` (rerun a test with waves). Depends on `svtypes` for the version table. |
| `xtask` | Repository automation: `cargo xtask ci`, `setup` (pinned tools), `doctor`. |

## Invariants
- `svtypes` and `simjig-bits` depend on no other workspace crate.
- `simjig-build` depends only on `svtypes` (and on `simjig-bits` as a dev-dependency, to
  compile the generated Rust in its tests); `cargo-simjig` depends on `svtypes` (the
  `svtypes::version` table and `svtypes::toolchain` rule) and nothing else in the
  workspace.
- Runtime crates never depend on build-time crates as normal dependencies. `simjig`'s
  build-dependency on `simjig-build` (where present) exists only to build its own test
  models.
- Nothing depends on `xtask`. Only `xtask` depends on `cargo-simjig` (for `cargo xtask
  doctor`); runtime crates never depend on it.
- `unsafe` exists only in `simjig::{model::abi, model::raw, kernel::waker, dpi::abi, fork}`
  and in the expansion of `simjig::__link_model!` (generated code's vtable symbol); every
  other module and crate forbids it.
- `simjig::kernel` imports nothing from `simjig::model`, so the scheduler can be tested
  against a fake model.
- No Rust reference to Verilator model memory ever exists. Testbench writes go through the
  write log, applied before each eval.
- Verilator's storage rule (width to `CData`/`SData`/`IData`/`QData`/`VlWide`) is written
  three times on purpose, once per side of a boundary that cannot share code:
  `svtypes::width::packed_storage` (build time: the IR, the generated Rust and C++),
  `simjig::model::abi::Storage::for_width` (run time: the C ABI mirror, which may not depend
  on a build-time crate) and `simjig::storage_for_width` in `simjig_model.h` (C++, where the
  shim's `static_assert`s and the probe check need it). They are kept equal by tests on
  both sides: `SIMJIG_SIGNAL` asserts the model's member types, `Signal::decode` rejects a
  table row whose storage disagrees with its width, and `simjig::find_probe`
  (`simjig_rt.cpp`) checks each probe's width against the `VLVT_*` type Verilator
  recorded for it.

## Single homes
One place per concept (constitution rule 6); everything else calls it.
| Concept | Home |
|---|---|
| Printing a simulation time in physical units | `simjig::kernel::time::TimeDisplay` (`SimTime::display`) |
| Rendering a failure | `Display` of `kernel::failure::Failure` and of `BlockedTask` (one blocked task); the harness's report (`harness::report::FailureReport`) builds on them |
| Seeds and random streams | `harness::seed`: `Seed` (root seed, test-name seed, Verilator seed), `Seed::rng` (named streams), `Rng` (xoshiro256**) |
| Stable hashing (FNV-1a 64) | `kernel::recorder::Fnv`: the trace hash, seeds, stream seeds, failure signatures |
| Checking a model's C vtable before use | `simjig::model::raw::check_vtable` |
