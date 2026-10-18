# s1fix-hash: merging s1fix-build and s1fix-codegen

## Seams between the branches
- `hash.rs` test initialiser lacked `CodegenConfig::{verilator_args, control_files}`.
  Fixed with `..CodegenConfig::default()`.
- Argument builder: s1fix-codegen's `verilator_args` reach the model build, the JSON pass
  and the layout check only through `DutConfig::design` -> `JsonPassOpts::design_args`
  (extra arguments last). `CodegenConfig::verilator_args` is the same vector, so the
  inlining check sees exactly the user arguments the model build gets (the builder adds
  no `-O`/`-f(no-)inline`/`--hierarchical` flags of its own). No change needed.
- Coverage: `emit.rs` treats any `--coverage*` extra argument as coverage for the shim;
  the covergroup check (`script::claim_covergroups`) reads `VM_COVERAGE` from the
  generated makefile, so both agree with what Verilator actually built.

## IR hash
- `codegen::lower` calls `hash::ir_hash`; the FNV hash is gone.
- `verilator_args` and `control_files` are **not** hashed (s1fix-codegen's intent: they
  only reject configurations and never change the generated Rust or C++). Test:
  `hash::tests::ir_hash_ignores_the_reject_only_fields`.
- `ir::SCHEMA` = 2 (for `unrepresentable`). The pinned hash `0x7602_a740_1ba1_fbe5` was
  reproduced in Python (the old pin `0x1f0d_95f3_7741_bfd0` reproduces with schema 1).

## Errors
- `Error::{Io, Spawn, Compile}` keep their cause in `source()` only.
- `Debug` of `Error`, `codegen::Error` and `PortMismatch` is `Display` plus a
  `Caused by:` chain, one cause per indented line. `cc::Error` cannot be constructed
  outside `cc`, so `Compile` is covered only by the shared attribute pattern.

## Rebuild tracking
- `PATH` is no longer in `BuildEnv::TRACKED_VARS`; the resolved executable is already
  `rerun-if-changed`. `cc` does not track `PATH` either.
- Still tracked and usually different outside `pixi run`: `CONDA_PREFIX` (FST library
  search), and `CXX`/`CXXFLAGS`/`LDFLAGS` if the pixi activation sets them. A test run
  outside pixi therefore still reruns the build script (and fails to find Verilator)
  when `CONDA_PREFIX` was set at build time. Removing that needs the lib dirs derived
  from the resolved Verilator instead of `CONDA_PREFIX`; not done here.
- A `PATH` change that resolves to a *different* Verilator is not noticed by itself.

## Docs
- `simjig` has no build-dependency on `simjig-build` on stage/1 (its build.rs uses `cc`
  directly); the ARCHITECTURE invariant says "where present".
