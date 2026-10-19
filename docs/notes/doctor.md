# doctor and setup: notes

## What exists
- `crates/cargo-simjig`: a binary (`cargo simjig doctor [--json]`) plus a library
  (`cargo_simjig::doctor`, `cargo_simjig::verilator`). `xtask` depends on the library, so
  `cargo xtask doctor` runs the same code. No new crate beyond `cargo-simjig`.
- `cargo xtask setup`: `pixi install --locked`, then `cargo install --locked --version`
  for each entry of `DEV_TOOLS` in `xtask/src/setup.rs`, the only place the cargo tool
  versions are written. Pins are the versions installed in `~/.cargo/bin` on 2026-10-17:
  cargo-nextest 0.9.148, cargo-deny 0.20.2, cargo-machete 0.9.2, typos-cli 1.51.1.

## Design
- Two phases. `facts.rs` is the only code that runs commands or reads the environment;
  `checks.rs` is pure functions from `Facts` to `Finding`. Every decision is unit-tested
  with fake command outputs. The FST check takes an `exists` function for the same reason.
- `Finding` constructors enforce the invariant "a fix command exactly when not OK".
- Severity: missing ccache and missing nextest are `WARN` (builds still work, slower or
  less isolated). A Verilator that cannot run or is not in `SUPPORTED`, an inconsistent
  `VERILATOR_ROOT`, missing FST sources, a C++ compiler that does not run, spaces in
  paths, and no usable tools at all are `FAIL`. `CXX` unset is `WARN` (the `cc` crate falls
  back to `c++`).
- `SIMJIG_UNTESTED_VERILATOR=1` downgrades an unsupported Verilator to `WARN`, as
  `proposal-C.md` describes for the strict front end.
- Why not serde_json for `--json`: the output is one fixed shape of strings and bools;
  escaping strings per RFC 8259 §7 is 15 tested lines. No dependency was added.
- Why not more crates: the checks live in the `cargo-simjig` library target.
- The doctor finds Verilator by the build's rule (`svtypes::toolchain::{locate, judge}`:
  `SIMJIG_VERILATOR`, then `$VERILATOR_ROOT/bin/verilator`, then `PATH`; a wrong explicit
  choice is an error). A `VERILATOR_ROOT` without `bin/verilator` is a `FAIL` of the
  `verilator` check (fix: unset it), because the build would not look further.
- The supported-Verilator table is `svtypes::version::SUPPORTED`, shared with
  `simjig-build`; the front end still decodes the whole 5.050-5.059 dialect range, but its
  error lists only `SUPPORTED`.

## Verilator 5.052 facts (checked against the installed launcher, `bin/verilator`)
- The launcher computes its root as `realpath($RealBin/../share/verilator)`. If
  `VERILATOR_ROOT` is set and its realpath differs, it exits 1 with
  `%Error: verilator: VERILATOR_ROOT is set to inconsistent path` on **every** command,
  including `--version`. So doctor runs all Verilator probes with `VERILATOR_ROOT` removed,
  asks `verilator --getenv VERILATOR_ROOT` for the binary's own root, and compares the
  canonicalised environment value to that.
- conda-forge's 5.052 has no `VERILATOR_ROOT` in the pixi activation; it is unset.
- FST tracing in 5.052 uses `include/verilated_fst_c.{h,cpp}` plus the C++ writer in
  `include/fstcpp/` (`fstcpp_writer.{h,cpp}`); there is no `gtkwave/fstapi.h` any more.
  The check looks for those four files.
- `verilator --version` prints `Verilator 5.052 2026-09-05 rev conda-forge build`; conda
  calls the same release `5.52`. `VerilatorVersion` parses both and displays `5.052`.

## Not verified
- The install branch of `cargo xtask setup` (a tool at the wrong version) was not run:
  every tool was already at its pin, so only the skip path ran. The decision is unit-tested.
- Linux: the doctor's tests pass on linux-aarch64 under pixi. conda-forge's `gxx` has no
  activation script, so `CXX` was unset there and the doctor warned; `pixi.toml` sets it
  (`[target.linux-*.activation.env]`). The `c++` fallback outside pixi was not checked.
- Windows: `is_executable` has a non-unix branch that was never compiled.

## Open issues
- The design (`proposal-C.md`) also lists "cache stale" and "fork available" checks; they
  need `simjig-build` and the runtime, and are not implemented.
