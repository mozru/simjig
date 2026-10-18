# s1fix-build: stage 1 review fixes (build side)

Facts checked against the installed Verilator 5.052.

## Verilator behaviour relied on
- `+define+K=V` splits at every `+`: `+define+DEPTH=4+4` defines `DEPTH=4` and `4`.
  `-DDEPTH=4+4` does not split. All passes now use `-D`.
- `--json-only` without `--no-timing` turns timing on; with `--no-timing`, `fork`, `#`
  and `@(e)` outside the top of a procedure fail with `NOTIMING`. `VERILATOR_TIMING`
  is defined only under `--timing`.
- Warning options: a later `-Wwarn-USERFATAL` downgrades an earlier
  `-Werror-USERFATAL`; `-Wno-USERFATAL` (anywhere) disables the warning, and only a
  later `-Wwarn-USERFATAL` re-enables it (`-Werror-` alone does not). The layout check
  therefore ends with `-Wwarn-USERFATAL --lint-only -Werror-USERFATAL`, after the
  user's arguments.
- `V<prefix>__Dpi.h` lists exports as `extern <type> <name>(...);` between
  `// DPI EXPORTS` and `// DPI IMPORTS`; a model without DPI has no such file.
- `verilated_covergroup` is in `VM_GLOBAL_FAST` of every covergroup model, with or
  without `--coverage`, and `verilated_covergroup.cpp` is the only runtime extra that
  reads a `VM_*` switch (`VM_COVERAGE`). `verilated_fst_c.cpp` and `verilated_cov.cpp`
  read none; `verilated.cpp` reads `VM_VPI` and `VM_SOLVER_DEFAULT` (unset).
- Cargo passes `rustc-link-lib` only to the library target when a package has one;
  `rustc-link-arg` reaches tests, examples, benches, binaries and cdylibs. Listing an
  archive both ways links each member at most once.

## Decisions
- **One argument builder:** `svtypes::verilator::JsonPassOpts::design_args` (timing,
  `--top-module`, `-I`, `-D`, `-G`, sources, extra arguments last). simjig-build builds
  the JSON pass, the model build and the layout check from `DutConfig::design`, so a
  define, parameter or timing mode cannot differ between IR and model.
- **One version table:** `svtypes::version` (`VerilatorVersion`, `SUPPORTED`,
  `supported_list`). cargo-simjig now depends on svtypes (both build time; svtypes
  depends on nothing). The front end still decodes the 5.050–5.059 dialect range, but
  its error lists only `SUPPORTED`.
- **One storage rule:** `svtypes::width::packed_storage` / `svtypes::ir::Storage`.
  `ir_ports` now returns `Result` and rejects width 0.
- **X/Z constants:** `Package::unrepresentable` / `Module::unrepresentable`
  (`ir::Unrepresentable { name, value, reason }`). Omitted from the JSON when empty, so
  existing IR (and every snapshot and IR hash) is unchanged. `ir::SCHEMA` was *not*
  bumped although the rule says any JSON change bumps it: a bump changes the IR hash in
  the codegen snapshots, which this branch does not own. Bump it together with the
  `ir_hash` switch below.
- **Build-script checks** live in `simjig-build/src/script.rs` (DUT name reuse, DPI
  export duplicates, covergroup `--coverage` mismatch), keyed by `OUT_DIR`.
- **IR hash:** `hash::ir_hash` (SHA-256 `KeyHasher`, config hashed field by field).
  `codegen::lower` must call it; until then it carries an `expect(dead_code)`.

## Not done here
- `codegen::Error` is not `#[non_exhaustive]` (codegen is not owned here).
- `Error::Io`, `Error::Spawn` and `Error::Compile` still print their source in
  `Display`, as before; only `Ir` and `Codegen` were in scope.
