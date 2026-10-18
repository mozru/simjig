# Architecture

Decision record: `docs/architecture/DECISION.md`.

**Scope:** making Verilator and Rust work together well (types, build, model access,
scheduling, time, errors, waves). Verification methodology and bus models (VIP, AXI,
scoreboards, constrained random, coverage closure, RAL) are out of scope; they belong to
projects built on top of simjig.

## Code map
| Crate | Owns |
|---|---|
| `svtypes` | Versioned type IR built from Verilator `--json-only`: widths, offsets, enums, params, ports, interfaces. Python/cocotb emitter. Its `version` module is the single table of supported Verilator versions (`VerilatorVersion`, `SUPPORTED`) for every build-time crate. |
| `simjig-bits` | Bit-vector values with SystemVerilog semantics (`Bits<N>`, `WideBits`, `Packed`). |
| `simjig-build` | Build time: one lowering feeding every emitter (Rust, C++ vtable, `.vlt`, wrapper SV, layout check), Verilator invocation, cache. |
| `simjig` | Runtime: `kernel` (scheduler, triggers, tasks, failures, recorder), `model` (Verilator backend over the C ABI), `signal` (typed handles), `dpi` (DPI-C imports implemented in Rust, per model instance), `harness` (`#[simjig::test]` support, seeds, reports, run dirs). |
| `simjig-macros` | `#[simjig::test]` attribute (thin; logic unit-tested). |
| `cargo-simjig` | User CLI: `cargo simjig doctor` (environment checks with fixes). Depends on `svtypes` for the version table. |
| `xtask` | Repository automation: `cargo xtask ci`, `setup` (pinned tools), `doctor`. |

## Invariants
- `svtypes` and `simjig-bits` depend on no other workspace crate.
- `simjig-build` depends only on `svtypes`; `cargo-simjig` depends on `svtypes` (the
  `svtypes::version` table) and nothing else in the workspace.
- Runtime crates never depend on build-time crates as normal dependencies. `simjig`'s
  build-dependency on `simjig-build` (where present) exists only to build its own test
  models.
- Nothing depends on `xtask`. `cargo-simjig` is a leaf tool; runtime crates never depend on it.
- `unsafe` exists only in `simjig::{model::abi, model::raw, kernel::waker, dpi::abi, fork}`; every other
  module and crate forbids it.
- `simjig::kernel` imports nothing from `simjig::model`, so the scheduler can be tested
  against a fake model.
- No Rust reference to Verilator model memory ever exists. Testbench writes go through the
  write log, applied before each eval.
