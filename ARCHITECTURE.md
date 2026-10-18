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
| `simjig-build` | Build time: one lowering feeding every emitter (Rust, C++ vtable, `.vlt`, wrapper SV, layout check), probe path resolution (`codegen/lower/path.rs`), the probe check against Verilator's scope table (`syms.rs`), interface-binding diagnostics (`iface_diag.rs`), Verilator invocation, the content-addressed caches (`cache.rs`: Verilator output; `front.rs`: each DUT's IR and layout-check verdict; `archive.rs`: model and runtime archives keyed by sources, flags, compiler identity and `-MD` dependencies; `manifest.rs`: the dependency manifests they share). |
| `simjig` | Runtime: `kernel` (scheduler, triggers, tasks, failures, recorder), `model` (Verilator backend over the C ABI), `signal` (typed handles), `harness` (`#[simjig::test]` support, seeds, reports, run dirs). |
| `simjig-macros` | `#[simjig::test]` attribute (thin; logic unit-tested). |
| `cargo-simjig` | User CLI: `cargo simjig doctor` (environment checks with fixes). Depends on `svtypes` for the version table. |
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
- `unsafe` exists only in `simjig::{model::abi, kernel::waker, fork}`; every other module and
  crate forbids it.
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
| Naming, slot, storage and access decisions | `codegen::lower` |
| The "did you mean" hint | `codegen::names::did_you_mean` |
| Two-step cache keys | `cache.rs` (Verilator output), `front.rs` (IR), `archive.rs` (archives), over the manifests of `manifest.rs` |
| What `simjig-build` writes into `OUT_DIR` | `emit.rs` (`OUT_DIR/simjig/`) |
| Environment variables that rerun the build script | `BuildEnv::TRACKED_VARS` |

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
  shim's `static_assert`s in `SIMJIG_SIGNAL`) and `vltype_for_width` (`find_probe`) derive.

They are kept equal by checks on both sides: `SIMJIG_SIGNAL` asserts the model's member
types, `Signal::decode` rejects a table row whose storage disagrees with its width, and
`syms.rs` and `find_probe` reject a variable whose `VLVT_*` type is not its width's
storage.
