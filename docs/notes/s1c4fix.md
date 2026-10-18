# s1c4fix: Stage 1 review cycle 4 fixes

Agent `s1c4fix`, branch `fix/s1c4fix` (from `stage/1`). Facts checked against the installed
Verilator 5.052 (pixi) and clang 21 (pixi's `CXX`) on macOS arm64.

## Probe paths through generate blocks
5.052 dumps the JSON after V3Width and before V3Begin. At that point generate blocks are
already elaborated `GENBLOCK` nodes (items in `itemsp`), checked on 5.052 output:
- a named block keeps its label (`g_x`); a false generate-if is gone;
- an unnamed block is `genblk<N>` with `unnamed: true`, N counting the module's generate
  constructs (IEEE 1800-2023 §27.6): the second `if` of a module is `genblk2`;
- a generate-for is unrolled into one block per iteration named `label[i]` (`g[0]`,
  `g[1]`), preceded by an `implied: true` block `label` that holds only the `GENVAR`;
- an instance array is one `CELL` with a `rangep`.
Verilator's scope table names these scopes the same way: `pipe.g_x.u_x`,
`pipe.genblk2.u_anon`, `pipe.g[0].u`, and a variable of a loop body is in scope
`pipe.g[0]` (`t`). So the indices *are* recoverable, and no deferral to the scope-table
check was needed.

IR schema 4: `Module::generate_blocks: Vec<GenerateBlock { name, instances, variables,
generate_blocks }>` (implied blocks and genvars skipped) and `Instance::array`. The
lowering's path walk (`codegen/lower/path.rs`) accepts `ident` or `ident[digits]`
segments, descends into instances (new module) or generate blocks (same module), and says:
- `` `g` is a generate-for loop; name one iteration's scope as Verilator elaborates it, e.g.
  `gen_pipe.g[0]` ``;
- `` `arr` is an instance array; instance arrays are not supported in probe paths `` (for
  `arr` and `arr[0]`; Verilator does register `arr[0]` scopes, but the brief asked for a
  refusal);
- a path ending at an instance or block: `` `lp.u_core` is an instance, not a variable ``;
- a module typo: `no module ... did you mean ...`.
Proven at run time too: `codegen_it::probes::probes_through_generate_blocks_bind_at_create`
writes and reads through `top.g_x.u.x`, `top.genblk2.u.x`, `top.g[1].one.u.x`, `top.g[0].t`.

Lowering probe errors are now `codegen::Error::Probe { path, reason }`, which `Dut::build`
reports as `Error::Config` "probe `p` (declared in build.rs): ...", like the scope-table check.

## Archive keys (models and runtimes, `archive.rs`)
Choice: the two-step depfile key, not hashing reachable directories. Hashing every directory
reachable from the sources and `-I` flags is both too much (a source in `src/` would hash
the crate; `-I/usr/include` the system) and too little (`#include "../x.h"` escapes it).
The depfile key is what the Verilator cache and ccache's direct mode do:
1. pre key: kind, ABI, simjig-build version, Verilator version and include dir, compiler
   path, args and identity, target, opt, debug, defines, sources (path and contents),
   include paths, and the *file names* in each source's directory, the include dirs and
   the dirs of `-I`/`-iquote`/`-isystem`/`-idirafter` flags (so a new shadowing header
   changes it);
2. `manifests/<pre>` lists every file the last compile read (`-MD`, so system headers are
   included: a header in an `-isystem` dir is tracked too); the full key adds their
   contents.
Remaining hole (shared with ccache): a new header that shadows another through a directory
whose listing is not hashed (a subdirectory of an include dir, the toolchain's own dirs).
Cost: `verilated.cpp` alone reads 1207 files (8.7 MB) under `-MD` (15 under `-MMD`); every
build re-hashes each archive's dependency list. Estimated at tens of ms per DUT, not
measured. Trade-off named: correctness over no-op build time.

The build script now also reruns on every compiler dependency outside the cache and
`OUT_DIR` (a header next to a recipe source was not tracked by Cargo at all before). Entry
directories hold only `lib<name>.a` (objects dropped; the archive is compiled under a
provisional name and renamed once the full key is known); an entry without its archive is a
miss and is replaced; `tmp/` work dirs older than an hour are removed by `Cache::open`.

Compiler identity: the executable `cc`'s tool path resolves to (through `PATH`, then
`canonicalize`), its size and mtime, and `<exe> --version` (status, stdout, stderr), run
once per (`OUT_DIR`, executable) per process. Keyed by `OUT_DIR` like `script.rs`, so tests
that stand for several build scripts in one process see a fresh answer.

Domain bumps: `simjig-archive-v4`, `simjig-archive-deps-v1`: every archive recompiles once.

## Other fixes
- `syms.rs`: insert calls are resolved after all `__Syms*` text is read. With
  `--output-split 1 --output-split-cfuncs 1`, 5.052 writes the tables and scope rows in
  `__Syms__Slow.cpp` and the calls in `__Syms__ctor__<n>__Slow.cpp`; the sorted file order
  happened to put `Slow` first, so the it-test (`probes_are_checked_in_a_split_scope_table`)
  passed before the fix too; the unit test with the calls first is the one that failed.
- Refused raw arguments: `--sc`, `--main`, `--binary`, `--lib-create`, `-o`,
  `--hierarchical`, and `--opt=value` forms of `--prefix`, `--Mdir`, `--top`,
  `--top-module`.
- Interface diagnostics moved to `iface_diag.rs`. 5.052's binding-mistake texts:
  `%Error-PINNOTFOUND: <elab>:L:C: Pin not found: 'in_bs'` (the hint comes from the
  `PINMISSING` pins of the same run), `%Error: ...: Port 'in_bus' expects 'bus_if' interface
  but pin connects 'other_if' interface`, `%Error: ...: Cannot find file containing
  interface: 'nope_if'`. An unreadable declaration location in the unbound-port case now
  gets its own line instead of dropping the whole message.

## Tests (each failed before its fix unless noted)
- svtypes: `runner::generate_blocks_record_their_hierarchy`;
- it: `probes::{a_probe_through_a_named_generate_block_builds,
  a_probe_through_an_unnamed_generate_block_builds,
  a_probe_through_a_generate_for_iteration_builds,
  generate_and_instance_array_mistakes_fail_the_build, probe_path_mistakes_fail_the_build
  (new cases), read_write_probe_of_a_localparam_fails_the_build (new headline),
  probes_are_checked_in_a_split_scope_table (passed before, see above)}`,
  `recipe::{editing_a_header_in_a_recipe_include_dir_recompiles_the_model (passed before:
  the behaviour existed; fails with the include-dir hashing disabled),
  editing_a_header_next_to_a_recipe_source_recompiles_the_model,
  a_new_compiler_version_recompiles_every_archive}`,
  `cache::{archive_entries_hold_only_the_archive, an_entry_without_its_archive_is_rebuilt,
  stale_work_dirs_are_removed}`, `ifaces::binding_mistakes_name_the_bind_iface_to_fix`,
  `emitter::codegen_errors_name_the_dut` (new headline);
- codegen_it: `probes::probes_through_generate_blocks_bind_at_create`,
  `errors::error_messages` (snapshot: module hint);
- unit: `syms::tests::calls_before_their_tables_and_scopes_resolve`,
  `iface_diag::tests::an_unreadable_declaration_keeps_the_other_ports`,
  `dut::tests::raw_flags_that_have_a_builder_switch_are_rejected` (new cases),
  `lower::path::tests::probe_segments_are_identifiers_or_loop_iterations`,
  `archive::tests::*`, `depfile::tests::compiler_shape`, `hash::tests::ir_hash_is_pinned`
  (re-pinned for schema 4, recomputed in Python).

## Not verified / open
- Linux, GCC (all runs macOS arm64 with clang 21).
- The no-op build cost of re-hashing `-MD` dependencies (estimated, not measured).
- A real build script rerun on a compiler dependency (Cargo's side of `rerun-if-changed`).
- `ARCHITECTURE.md` (not owned here) should list the new modules: `simjig-build`'s
  `archive.rs` (archive cache keys), `iface_diag.rs` (interface-port diagnostics) and
  `codegen/lower/path.rs` (probe path walk); `docs/notes/tracecov.md` and
  `docs/notes/svtypes.md` still say probe paths through generate blocks are unsupported.
