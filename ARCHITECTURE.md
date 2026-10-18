# Architecture

Decision record: `docs/architecture/DECISION.md`.

**Scope:** making Verilator and Rust work together well (types, build, model access,
scheduling, time, errors, waves). Verification methodology and bus models (VIP, AXI,
scoreboards, constrained random, coverage closure, RAL) are out of scope; they belong to
projects built on top of simjig.

## Code map
| Crate | Owns |
|---|---|
| `svtypes` | Versioned type IR built from Verilator `--json-only`: widths, offsets, enums, params, ports, interfaces. Python/cocotb emitter. |
| `simjig-bits` | Bit-vector values with SystemVerilog semantics (`Bits<N>`, `WideBits`, `Packed`). |
| `simjig-build` | Build time: one lowering feeding every emitter (Rust, C++ vtable, `.vlt`, wrapper SV, layout check), Verilator invocation, cache. |
| `simjig` | Runtime: kernel (scheduler, triggers, tasks), model FFI, signal handles, failures, recorder, seeded RNG streams. |
| `simjig-macros` | Proc macros (`#[simjig::test]`, derives). |
| `cargo-simjig` | User CLI: `cargo simjig doctor` (environment checks with fixes). |
| `xtask` | Repository automation: `cargo xtask ci`, `setup` (pinned tools), `doctor`. |

## Invariants
- `svtypes` and `simjig-bits` depend on no other workspace crate.
- `simjig-build` depends only on `svtypes`; build-time and run-time crates never depend on
  each other.
- Nothing depends on `xtask`. `cargo-simjig` is a leaf tool; runtime crates never depend on it.
- `unsafe` exists only in `simjig::{model::abi, kernel::waker, fork}`; every other module and
  crate forbids it.
- `simjig::kernel` imports nothing from `simjig::model`, so the scheduler can be tested
  against a fake model.
- No Rust reference to Verilator model memory ever exists. Testbench writes go through the
  write log, applied before each eval.
