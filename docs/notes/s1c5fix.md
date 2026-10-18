# s1c5fix: Stage 1 review cycle 5 fixes

Agent `s1c5fix`, branch `fix/s1c5fix` (from `stage/1`). Facts checked against the installed
Verilator 5.052 (pixi, conda-forge) and clang 21 (pixi's `CXX`) on macOS arm64.

## Model archives recompiled on every rerun (bug)
Cause: a model archive's pre key listed the directory of each source; the shim's was
`OUT_DIR/simjig/`, which holds every DUT's `<name>.rs` and shim and never dropped the shims
of earlier configurations. Any DUT edit changed every model's key, and during a cold build
the directory grew DUT by DUT, so the first rerun recompiled all models but the last.
Reproduced with `rebuild::a_rerun_compiles_only_the_edited_model` (a real `cargo build` of
`tests/fixtures/rebuild-crate`, three DUTs): before the fix, 5 archives after the first
no-change rerun (3 expected), 6 after editing one parameter (4 expected).

Fix: `Toolchain::is_generated` leaves the cache root and `OUT_DIR/simjig/` out of the
listings (nothing else writes there, and their file names follow from the key), and each
DUT's shim lives in `OUT_DIR/simjig/<name>/`, which holds only the current configuration's
shim. `<name>.rs` stays where users `include!` it.

## Compiler changed in place (risk)
`compile` emits `rerun-if-changed` for `archive::compiler_files`: the path the compiler is
found at (through `PATH` for a bare `CXX`), the executable it resolves to, and the same for
the program `cc` runs (a wrapper, `CXX="ccache c++"`). The ccache script simjig writes into
`OUT_DIR` is not listed (it is rewritten every run and would rerun every build). Tested
through a real `cargo build` (`rebuild::a_compiler_changed_in_place_reruns_the_build_script`:
`CXX` is a bare name found on `PATH`, a symlink to a script; appending to the script reruns
the build script; before the fix Cargo said Fresh).

## No-op rerun cost (risk)
Measured with a scratch crate whose `build.rs` builds counter (WIDTH=12), inv, flop, fifo
and comb_chain with the default emitter and times the five `build()` calls; a rerun is
forced by touching `duts/inv/inv.sv` (the content is unchanged). Per-phase times from
temporary `eprintln!` probes (not committed); the compiler setup row includes reading
`_classes.mk` and extracting the runtime sources.

| Phase (one DUT, full hit) | Before | After, first DUT | After, other DUTs |
|---|---|---|---|
| `Verilator::locate` (2 processes) | 57 ms | 57 ms | 0 |
| JSON pass (`design_ir`) | 57 ms | 0.7 ms | 0.2 ms |
| emitter and layout check (`--lint-only`) | 30 ms | 0.3 ms | 0.2 ms |
| compiler setup (`cc` family detection, SDK) | 114 ms | 124 ms | 0.5 ms |
| compiler `--version` | 25 ms (first only) | 25 ms | 0 |
| model archive lookup (~1000 deps) | 41 ms | 4 ms | 3 ms |
| libverilated lookup (~1200 deps) | 46 ms | 4 ms | 1 ms |
| **build-script body, 5 DUTs** | **1.80 s** | **0.24 s** | |

Totals over runs: before 1796, 1798, 1798, 1800, 1748 ms (first rerun after a cold build:
3571 ms, from the bug above); after 236, 238, 236, 236, 239, 236, 236 ms (first rerun
after a cold build: 254 ms). Target was 0.5 s.

What changed (one commit each, each with a test that failed before it):
- `manifest.rs`: archive manifests record each dependency's digest and status (size,
  mtime and ctime in ns, inode, device); a lookup hashes again only files whose status
  changed. A status is recorded only when both times are older than the compile's start by
  more than 3 s (FAT's 2 s granularity plus margin), ccache's "too new" rule, so a file
  written during or just before the compile is hashed every time. Domain
  `simjig-archive-deps-v2` (every archive compiles once).
- `archive::RUNTIMES`: the libverilated (and runtime extras) archive found for (`OUT_DIR`,
  cache root, pre key) is kept for the process. Model archives are not memoised: a test
  process may edit a header between two builds of one `OUT_DIR`.
- `Verilator::locate` memo per (`OUT_DIR`, `SIMJIG_VERILATOR`, `VERILATOR_ROOT`, `PATH`,
  untested); errors are not kept.
- `compile::seed`: every base `cc::Build` of a build script is a clone of one, so they
  share `cc`'s `BuildCache` (compiler family, flag support, SDK root). The test saw `cc` run
  `--version`, `-E detect_compiler_family.c` and `-?` on every DUT before.
- `front.rs`: the IR and the layout check verdict, keyed in two steps. The deps are the
  model verilation's (same sources, `-I`, `-D`, timing through `DutConfig::design`), minus
  the cache's `inputs/` and `verilator_bin`; so the entry is recorded after verilating, and
  the first build of a configuration always runs the JSON pass. The layout check marker is
  keyed on the full key and the check's text (the emitter may change the check without
  changing the IR).

Remaining cost on a full hit: the first DUT's Verilator lookup, compiler setup and
`--version` (about 205 ms of the 240 ms), then about 6 ms per DUT. Not done: passing the located Verilator's
version into `svtypes::verilator::json_pass`, which runs `verilator --version` once per
pass on a miss.

Hole shared with the verilation cache: an option file (`-f`) among the extra arguments is
hashed by path only when it is absolute, and Verilator's `.d` does not list it.

## VLVT types in the scope table (risk)
`syms.rs` now reads the `VLVT_*` field (`table_row` `f[2]`, `insert_call` `f[3]`):
anything but `VLVT_UINT8/16/32/64/WDATA` is refused with the SystemVerilog type ("`r` is a
`real` (Verilator stores it as VLVT_REAL); only packed variables ... can be probed"), and a
packed type must be the storage `packed_storage(width)` gives, as `find_probe` checks.
5.052 records `real r;` and `string s;` as `VLVT_REAL`/`VLVT_STRING` with 0 packed
dimensions (so they read as 1 bit). Tested by unit rows and
`probes::probes_of_a_real_or_a_string_fail_the_build` (`rtl/unpacked_vars.sv`), which built
before the fix.

## non_exhaustive (risk)
Added to `svtypes::ir::{TypeKind, VariableKind, ParamValue, ModuleKind}`,
`codegen::{ProbeType, SignalTarget, ItemKind, RustType, ConstValue, DutField}`,
`PortProblem`, `doctor::{Check, Status, Format}`. The lowering's matches on the IR enums
(another crate) report an unknown kind as `Unsupported` (`codegen::error::UNKNOWN_IR_KIND`).
Compile-fail doctests (`E0004`) on `ModuleKind` and `ProbeType`.
Left exhaustive on purpose: `ir::Direction` and `toolchain::Verdict` (matched in infallible
code and by both build-time crates, where a new variant should be a compile error to
decide, not a silent wildcard), `ir::Storage` and `PortDir` (Verilator's fixed sets),
`codegen::Access` (matched exhaustively by the codegen tests), `CacheStatus`.

## Nits
- ARCHITECTURE.md: a "Single homes" section (table of one-place concepts, the three probe
  validation layers, the storage-rule copies). The C++ copies now share one helper:
  `storage_for_width` moved to `simjig_rt.h`, and `storage_bytes` and the new
  `vltype_for_width` (used by `find_probe`) derive from it.
- Missing scope vs missing variable: one diagnosis each in `syms.rs` and `find_probe`. A
  hierarchical block's instance scope exists in the parent model; its variables do not, so
  the `hier_block` hint belongs to the missing variable (the build-time check had it on the
  missing scope). C++ test `missing_scopes_and_variables_are_told_apart`.
- `codegen::names::did_you_mean` is the one hint spelling (path.rs, syms.rs, iface_diag.rs).
- A path ending in an indexed segment (`top.g[0]`, `top.arr[1]`) gets the generate-block or
  instance-array message.
- `cpp/spike` marked historical in abi.md and its `run.sh`: it prints and asserts nothing
  (the `delay_internal` case ends in the expected abort), so adding it to `run.sh` would gate
  nothing; the behaviours are tested in `crates/simjig/tests/abi_counter.rs`.
- `CppRecipe::flags` reach the model archive only. Flags that must reach libverilated too
  are ABI switches (`-D_GLIBCXX_DEBUG`, `-D_LIBCPP_ABI_*`, `-stdlib=`, `-fsanitize=`,
  `-m*`/`--target`); they belong in `CXXFLAGS`, which reaches every unit of every DUT and is
  tracked. `CppRecipe::defines` and `include_dirs` still reach every unit (documented so;
  `global_sources` may need them).
- `BuildEnv::verilator_root` documents that conda-forge's `$VERILATOR_ROOT/bin/verilator`
  execs `../../../bin/verilator`, which refuses any root but its own: a copied root must be
  the whole prefix (checked: `share/verilator` alone fails with "Exec failed"; the original
  executable with a copied root with "inconsistent path"; `bin/verilator`,
  `bin/verilator_bin` and `share/verilator` copied together work).
- Stale docs fixed: `lower`'s `# Errors`, `codegen::Error::Config`'s example, two changelog
  entries (`Error::Probe`), and the "used to be `Error::Codegen`" wording.

## Not verified / open
- Linux, GCC; filesystems with coarse timestamps (the stat rule assumes at most 2 s).
- `docs/notes/wiring.md` (not owned here) still says the shim is in `OUT_DIR/simjig/`; it is
  now `OUT_DIR/simjig/<name>/<prefix>_shim.cpp`.
- The fixture-crate tests keep their target dir in `CARGO_TARGET_TMPDIR/fixture-rebuild`;
  the compiler test rewrites its wrapper each run, so each run compiles every archive of
  that fixture again (about 25 s).
