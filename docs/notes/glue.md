# Glue notes

An end-to-end check: the whole stack used as a user would. Checked against Verilator
5.052 (pixi), osx-arm64.

## What changed
| Area | Change |
|---|---|
| `codegen/rust.rs`, `rust/items.rs` | The generated file ends with `::simjig::__private::dut_model!(Dut, prefix, symbol = "simjig_vtable_<prefix>", precision_exponent)`, so it implements `DutModel` on its own. `Dut` and bundles derive `Clone, Copy, Debug`. Every name is an absolute path: macros (`::core::write!`, `::core::assert!`), derives (`::core::fmt::Debug`, ...), primitive types (`::core::primitive::u8`, ...). |
| `codegen::RustOptions { prefix, precision_exponent }` | New input of `emit_rust`. `codegen_emitter` fills it from `DutConfig::prefix()` and `Design::time_precision`. |
| `codegen_it` | Snapshots and `generated/*.rs` updated; the stub gained `dut_model!` (checks argument shapes and that `Dut` binds) and `Clone`/`Copy` handles; a `shadowed` module compiles four generated files under a glob import of hostile `write!`, `assert!`, `Debug`, `Clone`, `u8`, `u32`, `u64`, `u128`, `bool` (reverting any qualification fails to build: checked by hand on `enums.rs`). The generated modules are now reachable (`pub mod compile`), so generated code meets the workspace lints with no `unreachable_pub` exemption; only the stub and the case list keep one. |
| `crates/simjig/build.rs` | Builds all 15 test models with `simjig_build::Dut` (hand-written shims, `comb_chain.vlt` and the cc recipe are gone). Link directives are rewritten to `rustc-link-arg-tests`. Builds only with `SIMJIG_TEST_MODELS=1`, set in `.cargo/config.toml`. Writes `OUT_DIR/test_models.rs` (prefix and IR hash of every model) and `OUT_DIR/test_vtables/<name>.rs` (an `extern` block for the vtable symbol, whose `link_name` must be a literal). |
| `signal.rs` | `From<In<T>>`, `From<InOut<T>>`, `From<PublicRw<T>>` for `RawSig`: `simjig::clock(dut.clk, ns(10))`. Read-only handles have none (a `compile_fail` doctest). |
| `tests/e2e_dut` | User-style tests of inv, flop, struct_pipe, fifo, wide, fatal, iface_top. |
| `tests/e2e_golden` | All 13 cocotb goldens on real models, byte for byte. |
| `examples/quickstart` | inv + flop, `build.rs`, two test files, under `#![forbid(unsafe_code)]`. |
| `README.md` | Created, with "Quick start". |

`model_tests` now include the generated bindings (`include_dut!` at the crate root);
`model_tests::conformance` and its `conflog` moved to `e2e_golden` (the nine goldens it
replayed, plus four). `abi_counter` keeps its hand-instantiated vtables, of pipeline-built
models (`abi_counter`, `abi_ticker`).

## Results
- `e2e_dut`: 11 pass (1 ignored child). `e2e_golden`: **13 of 13 goldens match byte for
  byte**, including `two_clocks`, `cdc_stream`, `fifo_stream` and `derived_edge`; nothing
  is `#[ignore]`d. A mutation (one fewer `wclk` wait in `two_clocks`) fails at line 59.
- The goldens use in-run clocks (`simjig::clock(..).phase(ns(5)).start()` from `main` at
  t=0) where the earlier replay added clocks before `Sim::run`; the logs are identical.
- `$fatal` report (checked in a child process): header `FAIL fatal_tb::fatal_child
  seed=0x.. t=10000 ps [ModelFatal sig=..]`, message `$fatal in TOP.fatal at
  <path>/fatal.sv:31: fatal: kind=0 $fatal`, and a `repro:` line; rerunning with the
  printed `SIMJIG_SEED` gives the same seed and signature.
- `pixi run cargo xtask ci`: all steps pass (506 nextest tests).

## API friction found writing the tests as a user
1. **(Fixed since: `compile.rs` also emits every library as `rustc-link-arg`, so a
   library target is fine, for shared helpers.)** At the time, **a crate with a library
   target could not link its models into integration tests.**
   `Dut::build` prints `rustc-link-lib`, which Cargo gives to the library; an integration
   test that never names the library does not link it, so the vtable symbol is undefined
   (`ld: symbol(s) not found`). Without a library target the directives reach every target,
   which is why `examples/quickstart` has none. Proposed fix (simjig-build `compile.rs`):
   also emit the archives and `-lc++` as `cargo::rustc-link-arg` (bins, tests,
   examples, benches). Archives only load members on demand, so linking one twice is safe.
2. (Fixed since: `simjig::bits` re-exports the crate.) Users must depend on `simjig-bits` directly for `Bit`, `Bits`, `WideBits`: `simjig`
   re-exports them only under `__private::bits`. A public `simjig::bits` (or top-level
   re-exports) would remove one dependency from every testbench.
3. (Fixed since: the free triggers take `impl Into<RawSig>` too.) `simjig::clock` takes `impl Into<RawSig>`, but `cycles`, `rising`, `falling`, `changed`
   (free functions) take `RawSig`: `cycles(dut.clk.raw(), 2)` next to `clock(dut.clk, ..)`.
   `cycles` should take `impl Into<RawSig>` too (and outputs need `.raw()` anyway).
4. (Fixed since: `TimeDisplay` prints the largest exact unit.) Failure times print in ticks of the precision (`t=10000 ps`), not the largest exact unit
   (`10 ns`); the timeout attribute takes `"1us"`.
5. Signal names of an interface-ported DUT are the wrapper's
   (`simjig_wrap_iface_top.out_bus_valid`), which is what reports and `name()` show; the
   user wrote `out_bus.valid`.
6. Clocks start high by default (`phase 0`); cocotb's common `start_high=False` is
   `.phase(period / 2)`. Not wrong, but worth a line in the `clock` docs.
7. `include_dut!` expands to `pub mod`; called inside a private module, the generated `pub`
   items trip `unreachable_pub`. At the crate root (the documented use) it is clean.

## Decisions
- **`simjig` build-depends on `simjig-build`.** ARCHITECTURE.md says build-time and run-time
  crates never depend on each other; this is a build-dependency of the runtime crate's own
  test models only (no runtime link: directives are `-tests` only, and the models build
  only under `SIMJIG_TEST_MODELS=1`). The alternative, a separate test-only crate, would
  move the runtime's tests away from the runtime.
- The `iface_top` "writing an `Out` field does not compile" check is compositional: a test
  binds every bundle field with an explicit `In<..>`/`Out<..>` type (fails to compile if
  codegen types one differently), and `simjig::Out` carries a `compile_fail` doctest for
  `set`. trybuild was tried: the case must include `OUT_DIR` files, so it has to be
  generated at run time outside the crate, where trybuild does not normalise paths, and the
  expected stderr quotes `signal.rs` internals (`handle!` docs). Too brittle to keep.
- The `$fatal` report is checked in a child process (this test binary, `--ignored --exact`)
  because `should_panic` can match only one substring; libtest prints the block on stdout.
- Generated `Dut` code relies on the linker dropping unreferenced `dut_model!` glue:
  `model_tests::codegen` includes all 14 `codegen_it/generated` files whose vtable symbols do
  not exist, and links on macOS (`-dead_strip`). Not verified on Linux (`--gc-sections`).

## Not verified / open
- Linux; a cold build (only warm-cache runs: `simjig`'s build script takes about 35 s
  with the cache warm, mostly the 15 JSON passes and layout checks).
- Docs owned by others mention the old hand-written path: `docs/notes/model.md` ("built by
  `crates/simjig/build.rs` (hand-written shim ...)", `model_tests::conformance`).
- No CHANGELOG exists for the generated-code change (constitution rule 4); the snapshot
  diffs are the reviewed record.
