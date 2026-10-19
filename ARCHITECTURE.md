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
| `simjig-build` | Build time: one lowering feeding every emitter (Rust, C++ vtable, `.vlt`, wrapper SV, layout check, DPI-C trait and trampolines in `codegen::dpi`, with `Dut::dpi_in_c` leaving chosen imports to C; parameter sweeps: `Sweep` in `sweep.rs`, their lowering in `codegen/lower/sweep.rs` and Rust in `codegen/rust/sweep.rs`), probe path resolution (`codegen/lower/path.rs`), the probe check against Verilator's scope table (`syms.rs`), interface-binding diagnostics (`iface_diag.rs`), Verilator invocation, the content-addressed caches (`cache.rs`: Verilator output; `front.rs`: each DUT's IR and layout-check verdict; `archive.rs`: model and runtime archives keyed by sources, flags, compiler identity and `-MD` dependencies; `manifest.rs`: the dependency manifests they share), per-build-script checks (`script`: DUT names, DPI exports, covergroups, one `-rpath` per directory). |
| `simjig` | Runtime: `kernel` (scheduler, triggers, tasks, failures, recorder; refuses every entry point inside a DPI-C call; `kernel::trigger::TriggerExt`: `.read_write()`/`.read_only()` on edge, cycle and timer triggers), `prelude` (the one glob import of a testbench), `model` (Verilator backend over the C ABI; `model::trace`: FST waves sampled per timestep and moved to a per-branch file in a fork child; coverage counters via `Sim::coverage`), `signal` (typed handles; `Value` and `Vector`, the bounds generic test bodies use), `dpi` (DPI-C imports implemented in Rust, dispatched per model instance; a panic is `FailureKind::DpiPanic`), `harness` (`#[simjig::test]` support, seeds, reports, run dirs; `harness::waves`/`replay`: waves on failure from a traced replay in a child process; `harness::multi`: several DUTs per test as one `DutModel` tuple, `Lockstep`, `Mirror`), `fork` (`run_from_checkpoint`: branches forked from a quiet point, results over a socket, `ForkHook`s for resources a child must not share). |
| `simjig-macros` | `#[simjig::test]` attribute, `duts = [..]` included (thin; logic unit-tested). |
| `cargo-simjig` | User CLI: `cargo simjig doctor` (environment checks with fixes), `cargo simjig waves` (rerun a test with waves) and `cargo simjig show` (print a DUT's generated module, found through Cargo's `build-script-executed` messages). Depends on `svtypes` for the version table. |
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
- `unsafe` exists only in `simjig::{model::abi, model::raw, kernel::waker, kernel::cell, kernel::models (two `Direct` accesses), kernel::state (the installed kernel's address), dpi::abi, fork}`
  and in the expansion of `simjig::__link_model!` (generated code's vtable symbol). The
  workspace denies `unsafe_code` (`[workspace.lints.rust]`, checked by `xtask`'s
  `every_workspace_member_denies_unsafe`); those modules, and the tests that drive the C
  ABI or call `std::env::set_var`, opt in with `#[expect(unsafe_code, reason = ..)]`.
- `simjig::kernel` imports nothing from `simjig::model`, so the scheduler can be tested
  against a fake model.
- No Rust reference to Verilator model memory ever exists. Testbench writes go through the
  write log, applied before each eval.

## Single homes
One place per concept (constitution rule 6). Where a rule has to be written more than once,
because the copies sit on two sides of a boundary that cannot share code, every copy is
listed here with what keeps the copies equal.

| Concept | Home |
|---|---|
| Supported Verilator versions; which Verilator to run and accept | `svtypes::version`; `svtypes::toolchain::{locate, judge}` |
| Widths and packed storage at build time | `svtypes::width` |
| The design's Verilator arguments (JSON pass, layout check, model) | `DutConfig::design` (`svtypes::verilator::JsonPassOpts::design_args`) |
| Reading Verilator option files (`-f`/`-F`, nested; as bytes) and resolving their paths | `simjig_build::optfile::traverse` (visited by `find`, `inputs` and `untracked`); `Dut::file_list` passes through `DutConfig::option_args` |
| Defaulting a bound interface's parameters to the DUT's same-named `.param` (a binding value wins), and warning when the two stay apart | `simjig_build::pipeline::elaborate`; `pipeline::IfaceParamMismatch` (its warning, and its hint on a width error) |
| Naming, slot, storage and access decisions | `codegen::lower` |
| What a sweep's members share, and its `Ports`, bundle and fields traits and runtime re-exports (associated types, bounds, names) | `codegen::lower_sweep` (`codegen/lower/sweep.rs`) |
| Which DUTs a sweep has, and that its name is no DUT's | `simjig_build::script::claim_sweep` (and `claim_name`, its other side) |
| The "did you mean" hint | `codegen::names::did_you_mean` |
| Two-step cache keys | `cache.rs` (Verilator output), `front.rs` (IR), `archive.rs` (archives), over the manifests of `manifest.rs` |
| An input edited during a run | `manifest::{Snapshot, Manifest::record}`, `cache::keep_unpublished` |
| What `simjig-build` writes into `OUT_DIR` | `emit.rs` (`OUT_DIR/simjig/`; a sweep's module and member files in `emit::place_sweep`, its failure stub in `emit::stub_failed_sweep`); the path `OUT_DIR/simjig/<name>.rs` is read back by `simjig::include_dut!` and by `cargo-simjig`'s `show` (which may not depend on `simjig-build`), kept equal by their docs naming `emit.rs`; after a failed build that file is a `compile_error!` stub (`emit::stub_failed`) |
| The optional-DUT gate `cfg(simjig_dut_<name>)` | `emit::cfg_directives` |
| Environment variables that rerun the build script | `BuildEnv::TRACKED_VARS` |
| Which paths a DUT's `rerun-if-changed` lines track (built or failed) | `dut::reruns` over `dut::tracked_files` |
| Printing a simulation time in physical units | `simjig::kernel::time::TimeDisplay` (`SimTime::display`) |
| Rendering a failure | `Display` of `kernel::failure::Failure` and of `BlockedTask` (one blocked task); the harness's report (`harness::report::FailureReport`) builds on them |
| Seeds and random streams | `harness::seed`: `Seed` (root seed, test-name seed, Verilator seed), `Seed::rng` (named streams), `Rng` (xoshiro256**) |
| A model's initial values (random or zero) | Chosen by `harness::runner::init_values` (`init` argument, else `SIMJIG_INIT`, else zero) as `harness::dut::InitValues`; seeded by `Seed::init_seed` (one stream per model, `ModelOptions::for_model`; models of one design, `DutModel::design`, share the first one's in `harness::multi::model_options`); applied at create by the shim's `Vtable::construct_randomized` (`simjig_model.h`) through `simjig_create_opts.init`/`init_seed`; possible only for `--x-initial unique`, which `simjig_build::XInitial` alone sets and `SIMJIG_FEATURE_RANDOM_INIT` reports |
| Stable run-time hashing (FNV-1a 64) | `kernel::recorder::Fnv`: the trace hash, seeds, stream seeds, failure signatures |
| Stable build-time hashing (SHA-256 over length-prefixed fields; never `Debug` output) | `simjig_build::hash::KeyHasher`: cache keys, the model prefix and its claim (`DutConfig::model_digest`), the IR hash (`hash::ir_hash`, its first 64 bits) |
| Checking a model's C vtable before use | `simjig::model::raw::check_vtable` |
| Trace file names: `<stem>.fst` for the first model, `<stem>.model-<i>.fst` for model `i`, then `.branch-<j>` for fork branch `j` (`<stem>[.model-<i>].branch-<j>.fst`; checkpoints cannot nest) | Built by `harness::dut::ModelOptions::for_model` (`.model-<i>`) and `model::trace::branch_path` (`.branch-<j>`); parsed back only by `harness::replay::trace_identity` |
| The time grammar of a waves window (`<t0>..<t1>`) | `simjig::harness::waves::parse_window` (`SIMJIG_WAVES=window:`, whole numbers of a unit). `cargo-simjig`'s `waves::parse_window` is a deliberate second copy, since `cargo-simjig` may not depend on `simjig`: it also takes decimals (`1.5us`) and turns them into the harness's form, so it accepts a superset and emits only what the harness parses |
| Scheduler semantics | `tests/conformance/README.md` (normative, DECISION §0) |
| What a testbench glob-imports | `simjig::prelude`; a sweep module re-exports the same `Value`, `Vector` and `Packed` items (`codegen::lower_sweep`), so both globs coexist (`examples/lib/tests/prelude.rs`) |
| The recommended project layout | `examples/quickstart` (lib target with `pub use simjig::prelude::*;` and the `include_dut!`s); the README's "Project layout" quotes it, checked by `xtask`'s `the_readme_project_layout_is_the_quickstart`; the examples' lint tables mirror the workspace's (`example_lints_mirror_the_workspace`) |

### Probe validation
A probe is checked in three layers, each against what only it can see:
1. **the lowering's path walk** (`codegen/lower/path.rs`), against the IR, before
   verilating: the path's spelling, each instance and generate block on it, the declaring
   module, that the variable exists, and that a writable probe is not a parameter or
   localparam;
2. **the scope-table check** (`syms.rs`), against `V<prefix>__Syms*.cpp`, right after
   verilating: that the scope and variable are public, the variable's `VLVT_*` type (a
   packed vector, in the storage its width has), its width, that it is packed, and that a
   writable probe is `public_flat_rw` and not a parameter;
3. **the create-time backstop** `simjig::find_probe` (`cpp/simjig_rt.cpp`), against the
   live scope table: the same checks as layer 2, for shims that `simjig-build` did not
   check (hand-written, or a model built elsewhere). A missing scope and a missing variable
   get the same diagnosis here as in layer 2.

### Verilator's storage rule
Width to `CData`/`SData`/`IData`/`QData`/`VlWide` is written three times on purpose, once
per side of a boundary that cannot share code:
- `svtypes::width::packed_storage` (build time: the IR, the generated Rust and C++, and
  layer 2's `VLVT_*` check in `syms.rs`);
- `simjig::model::abi::Storage::for_width` (run time: the C ABI mirror, which may not
  depend on a build-time crate);
- `simjig::storage_for_width` in `cpp/simjig_rt.h` (C++), from which `storage_bytes` (the
  shim's `static_assert`s in `SIMJIG_SIGNAL`) and `vltype_for_width` (`find_probe`) derive;
- `simjig_bits::Packed::Raw` (the Rust value types: `u8`/`u16`/`u32`/`u64`/`[u32; W]` per
  width, in `simjig-bits`, which depends on nothing), pinned by
  `simjig-bits/tests/it/vectors.rs` `raw_types_match_verilator`; a signal-table row's storage
  is checked against `Storage::for_width` when the table is decoded (`Signal::decode`).

### The C ABI
`cpp/simjig_abi.h` is the C ABI; `simjig::model::abi` mirrors it in Rust (`#[repr(C)]`).
The copies are kept equal by the compile-time size and offset asserts in `abi.rs` and by
`crates/simjig/tests/abi_counter.rs`, which compares every mirrored field's offset and
size with the C++ compiler's (`simjig_rt_abi_fields`, `simjig_rt_abi_layout` in
`cpp/simjig_rt.cpp`); `cpp/tests/main.cpp` checks that that table covers every field.

They are kept equal by checks on both sides: `SIMJIG_SIGNAL` asserts the model's member
types, `Signal::decode` rejects a table row whose storage disagrees with its width, and
`syms.rs` and `find_probe` reject a variable whose `VLVT_*` type is not its width's
storage.
