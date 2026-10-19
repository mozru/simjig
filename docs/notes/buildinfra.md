# buildinfra notes (`simjig-build` orchestration)

## Shape
`Dut` (builder) records a `DutConfig`; `Dut::build()` = `BuildEnv::from_cargo()` +
`Dut::build_in(&env)` + print directives. `build_in` runs, in order:
`Verilator::locate` (find + version gate) → `Cache::verilate` (content-hash cache) →
`read_header_ports` (+ `check_ports` if `expect_ports` was given) → `compile::compile`
(libverilated cached once per configuration, model into `OUT_DIR`) → directives.

Emitter seams are data, not a trait: emitters read `DutConfig` (probes, iface bindings,
`DutConfig::prefix()`, computable before building), and feed back through
`Dut::verilator_input` (generated `.vlt`/SV), `Dut::recipe(CppRecipe)` and
`Dut::expect_ports`. Why not a trait: there is no second implementation, and every
emitter runs before verilation (they need the IR, not Verilator's output), so plain inputs
suffice. All three inputs are hashed into the cache keys.

## Cache
Root: `SIMJIG_CACHE_DIR`, else absolute `$CARGO_TARGET_DIR/simjig-cache`, else the nearest
ancestor of `OUT_DIR` holding Cargo's `CACHEDIR.TAG` (the target dir, also with
`--target <triple>`), else `OUT_DIR/simjig-cache`. Two-step key like ccache direct mode,
because only Verilator knows which files it `` `include``s:
- pre key = ABI_VERSION, crate version, full `verilator --version` line, every argument,
  contents of every existing absolute-path argument (sources, generated inputs, `-f`
  files), file names in each `-I` directory (shadowing);
- `manifests/<pre>` lists the files the last such build read (from `--MMD`'s `.d`);
  full key = pre key + their current contents. `verilator_bin*` is excluded (covered by
  the version line; hashing it would cost ~10 MB per build).
Entries appear only by `rename` of a complete work dir; the loser of a race discards its
copy. Generated inputs live content-addressed under `inputs/`, so their paths (which are
Verilator arguments) are the same in every crate.

## Facts found (Verilator 5.052, conda-forge, macOS arm64)
- Generated C++/headers contain no absolute paths, so verilating into a temp dir and
  renaming is safe (only `__verFiles.dat` and `__ver.d` mention the Mdir).
- Header port macros: `VL_IN8/16/64/W` and **`VL_IN` (no suffix) for 17-32 bits**; there
  is no `VL_IN32`. Ascending ranges are normalised (`[0:7]` → `7,0`). Unpacked-array ports
  (`VlUnpacked<..> &arr;`) and `real` ports (`double &r;`) have no macro and are not
  `HeaderPort`s: the IR side must leave them out of `expect_ports`.
- `_classes.mk` is authoritative for the file lists: `VM_GLOBAL_*` gives the runtime
  (e.g. `verilated verilated_threads`, plus `verilated_cov verilated_covergroup
  verilated_fst_c` with `--coverage --trace-fst`); `VM_CLASSES_*`/`VM_SUPPORT_*` the model.
- `sc_time_stamp()` is a weak reference in libverilated; Apple's linker rejects it unless
  every final link passes `-Wl,-U,__Z13sc_time_stampv` (as `verilated.mk` does). Build
  scripts cannot add link args to dependent crates, so simjig defines `VL_TIME_CONTEXT`
  (time comes only from `VerilatedContext`, which matches "the testbench owns time").
- `cc` reads `CXX` only from the process environment and only recognises a ccache
  wrapper inside it. To use pixi's ccache automatically, simjig writes a two-line
  `OUT_DIR/simjig-cxx-ccache` script (`exec ccache <real compiler> "$@"`) and passes it as
  the compiler; the libverilated key uses the real compiler path so the cache stays shared.
  Opt out with `SIMJIG_CCACHE=0`; skipped when `CXX` already has a wrapper or arguments.
- `+define+K=V` splits at every `+`: `+define+DEPTH=4+4` defines `DEPTH=4` and `4`.
  `-DDEPTH=4+4` does not split, so every pass uses `-D`.
- `--json-only` without `--no-timing` turns timing on; with `--no-timing`, `fork`, `#` and
  `@(e)` outside the top of a procedure fail with `NOTIMING`. `VERILATOR_TIMING` is defined
  only under `--timing`.
- `VM_TIMING` in `_classes.mk` cannot confirm timing: it is 0 for a `--timing` model without
  timing constructs, whose header still declares `eventsPending`. So the timing flags are
  refused wherever they appear (arguments and option files) rather than checked after.
- `-G`/`-pvalue+` also reach the layout check, which then fails with `Parameters from the
  command line were not found in the design`; hence `.param()` only.
- `V<prefix>__Dpi.h` lists exports as `extern <type> <name>(...);` between
  `// DPI EXPORTS` and `// DPI IMPORTS`; a model without DPI has no such file.
- `verilated_covergroup` is in `VM_GLOBAL_FAST` of every covergroup model, with or without
  `--coverage`.
- Verilator's `.d` lists every option file it read (`-f`, `-F`, nested ones too), so their
  contents are in the full key even when the pre key cannot hash them (a relative path).
- Cargo passes `rustc-link-lib` only to the library target when a package has one;
  `rustc-link-arg` reaches tests, examples, benches, binaries and cdylibs. Listing an
  archive both ways links each member at most once.
- A build script that exits non-zero has its whole stdout dumped under `--- stdout`,
  `cargo::` lines included, and Cargo reruns a failed script on every build whatever it
  printed. Its `cargo::warning`s are printed above `--- stdout`, both when it fails and when
  it handles the error and exits 0 (cargo 1.99.1).

## Archive keys: why the depfile key
Archives use the same two-step key as the Verilator cache (`archive.rs`). Why not hash every
directory reachable from the sources and `-I` flags: that is too much (a source in `src/`
would hash the crate, `-I/usr/include` the system) and too little (`#include "../x.h"`
escapes it). Remaining hole, shared with ccache: a new header that shadows another through a
directory whose listing is not hashed (a subdirectory of an include dir, the toolchain's own
dirs). Cost: `verilated.cpp` alone reads 1207 files (8.7 MB) under `-MD` (15 under `-MMD`);
the stat rule in `manifest.rs` keeps a lookup from re-hashing them.

Compiler identity: the executable the tool path resolves to (through `PATH`, then
`canonicalize`), its size and mtime, and `<exe> --version` (status, stdout, stderr), run once
per (`OUT_DIR`, executable) per process. Keyed by `OUT_DIR`, so tests that stand for several
build scripts in one process see a fresh answer. `CppRecipe::flags` reach the model archive
only; flags that change the C++ ABI belong in `CXXFLAGS`, which reaches every unit and is
tracked.

## No-op rerun cost
A rerun that changes nothing (five DUTs: counter, inv, flop, fifo, comb_chain) took 1.80 s
of build-script time before the memoisation below and takes about 0.24 s after. What
remains on a full hit is the first DUT's Verilator lookup (two processes, 57 ms), `cc`'s
compiler setup (family detection, SDK; about 120 ms) and `--version` (25 ms), then about
6 ms per DUT. What made the difference:
- the stat rule in `manifest.rs`: model archive lookup (about 1000 deps) from 41 to 4 ms,
  libverilated (about 1200) from 46 to 4 ms;
- the front cache (`front.rs`): the JSON pass from 57 to under 1 ms, the layout check
  from 30 to under 1 ms;
- `Verilator::locate` memoised per (`OUT_DIR`, `SIMJIG_VERILATOR`, `VERILATOR_ROOT`, `PATH`,
  untested); errors are not kept;
- one seed `cc::Build` per build script (`compile::seed`), so every DUT shares `cc`'s
  `BuildCache` instead of rerunning `--version`, `-E detect_compiler_family.c` and `-?`;
- `archive::RUNTIMES`. Model archives are not memoised: a test process may edit a header
  between two builds of one `OUT_DIR`.

Not done: passing the located Verilator's version into `svtypes::verilator::json_pass`,
which runs `verilator --version` once per pass on a miss.

## Inputs edited during a run
The rule is in `manifest.rs`; details it relies on:
- the start is a tick of the filesystem clock after the work directory was created.
  Linux stamps files with a coarse clock that can lag `SystemTime::now()`, and the
  generated `.vlt`, written just before the run, got the work directory's exact time, so
  every fresh Linux build counted as changed. `Snapshot::start` now waits for the clock to
  tick past the work directory (at most 50 probes, 1 ms apart; immediate on macOS);
- contents are hashed before the times are read, so an edit in between is caught;
- unpublished output goes to `OUT_DIR/simjig-unpublished/<label>` (it must outlive the
  build script, the link reads it; if the rename fails across filesystems it stays in the
  cache's `tmp/`). A good build of the same model removes its leftover there, except the
  Verilator runtime's, which an earlier DUT of the same script may link;
- an archive compiled from an unpublished verilation is not published either: its key
  names a one-off path that no later build finds;
- the IR is recorded only after the model checks pass, so a build that fails the header,
  probe or DPI check leaves no IR cached;
- known false positives: a file with a future time (clock skew) is never cached; on a
  coarse-clock filesystem a file written in the same tick as the start costs one extra
  rebuild.

## Reruns
- Dependencies under `<cache>/inputs/` are not `rerun-if-changed`: they derive from tracked
  configuration and sources, and the run that stores them writes them after the build
  script started, which Cargo saw as a change on the next build.
- One rule for the built and the failed path (`dut::reruns` over `dut::tracked_files`).
  Why not "track only the `-MD` headers": the C++ compile's dependencies are `cc`'s, not
  Verilator's, and the failure path has neither. A directory is tracked by its direct
  files, never an ancestor of `OUT_DIR` (a raw `include_dirs` entry `"."` made Cargo scan
  the package and its `target/` on every build).
- A failed build tracks its configuration, not what Verilator read (the failure may come
  before any Verilator run): the option files and the paths in them and in
  `verilator_args` (positional words with a source suffix, `-v`, `-y`, `-I<dir>`,
  `+incdir+`), resolved like `V3Options::parseFileArg`. Positional words are recognised by
  suffix because only Verilator's option table knows which options take a value
  (`--timescale 1ns/1ps`). Lost on that path: a file added later to an `-y`/`+incdir+`
  directory, files in its subdirectories, a source without a known suffix, `$VAR` in paths.
- The toolchain's own headers are not rerun paths (`archive::toolchain_dirs`). On macOS
  (conda clang 21) this took `simjig`'s test build script from 1,268 distinct paths to 69;
  the three that remain outside Verilator's are conda's `zlib.h`, `zconf.h` and `lz4.h`
  (`fstcpp_writer.cpp` includes them), on purpose. Risk: a compiler installed inside a
  project tree (`<proj>/tools/bin/cc`) hides edits to `<proj>/tools/include` from reruns
  (the archive keys still see them on the next build).

### Reruns before a build error
A failed DUT's reruns matter only when the script handles the error and exits 0; Cargo
reruns a failing script anyway. But `Dut::build` returns before the script decides, and
nothing in the process learns the exit status later (Rust runs no static destructors, and
`atexit` gets no status). Each way to hold the lines back loses reruns of a handled failure
(rule 4):
- flush on the next `Dut::build`: loses them when the last DUT fails and is handled;
- flush on `Drop` of the `Error` unless it was `Debug`-formatted: a `Drop` impl breaks
  by-value field moves out of the public enum (E0509), and an error logged with `{e:?}`
  would silently lose its reruns;
- an explicit "handled" call: existing handled scripts would silently stop rerunning.

So the reruns stay, and `Dut::build` prints `Error::summary` as `cargo::warning=simjig: ..`
lines before any of a failing DUT's directives; Cargo shows them above the dump.

## A failed DUT: stub plus `cfg`
Why both, and why not simpler:
- deleting the stale `OUT_DIR/simjig/<name>.rs` gives "couldn't read .../x.rs", which does
  not say the DUT failed; the stub costs the same and names the DUT and its error;
- detecting the stub in `include_dut!` would need a marker the macro can test at expansion
  time; the stub's own `compile_error!` already is that;
- the stub alone gives no way to build without the DUT. A `cfg` gates every item that names
  the model. Letting each user print their own cfg from the `Result` works too, but every
  script would repeat it, and `check-cfg` on the failure path is easy to forget. A failed
  build prints the `check-cfg` only, so `#[cfg(simjig_dut_<name>)]` raises no
  `unexpected_cfgs` either way;
- no stub when an earlier DUT of the same script claimed the name (that DUT owns the file),
  or for a non-identifier name. Writing the stub is best effort.

## `Dut::file_list`: why not simpler
- Count an `-f` among `verilator_args` as sources: extra arguments go after the sources, so
  a package in the list is read after a source importing it. Moving every `-f` before the
  sources would change what existing option files (lint flags placed last on purpose) mean.
- Expand the list into `.sources`: it would drop the options a list carries (`+incdir+`,
  `-D`, `-y`) or need a second copy of Verilator's option parser, and the cache would hash
  the expansion instead of what Verilator read.

So the list is passed as `-f`/`-F` in `JsonPassOpts::design_args` (after `-G`, before the
sources). The cache keys see it for free: the pre keys hash every absolute file among the
arguments, and Verilator's dependencies list the list and the files in it.

## Errors
- `pipeline::blame_param` names the `.param` or binding whose value the first error is in,
  by location (a `.NAME(` of a generated top, or a `-G` parameter's declaration). It is a
  heuristic; an error elsewhere is left as it was.
- Package hints key on 5.052's messages: `Can't find typedef`, `Package/class for .. not
  found`, and `.. before declaration (IEEE 1800-2023 6.18)` (which `import p::*` of a
  package read later or not at all gives).

## Exhaustive enums
Public enums that may grow are `#[non_exhaustive]`; the lowering reports an unknown IR kind
as `Unsupported`. Left exhaustive on purpose: `ir::Direction` and `toolchain::Verdict`
(matched in infallible code by both build-time crates, where a new variant should be a
compile error to decide, not a silent wildcard), `ir::Storage` and `PortDir` (Verilator's
fixed sets), `codegen::Access` (matched exhaustively by the codegen tests).

## Linux
Run on linux-aarch64 (Ubuntu 26.04 arm64 in the Lima VM of `ci/lima`, 4 vCPUs, 8 GiB);
linux-64 is locked in `pixi.lock` but was not run. Results: `cargo xtask ci` passes (991
nextest tests, about 5 min warm), the conformance goldens reproduce exactly, Miri passes,
and ASan/UBSan run the spike clean. Checked and fine: `fork()` checkpoints, `threads(1)`
detection, `-MD` dependency paths, path canonicalisation, glibc `strtol`, the C++ tests.

Failures found and fixed:
1. Models did not link (`undefined reference to symbol 'exp2@@GLIBC_2.17'`, then
   `__aarch64_ldadd4_acq_rel`). rustc links with `cc -nodefaultlibs`; std's `-lm -lpthread`
   and `compiler_builtins` come before the `rustc-link-arg` archives, where `--as-needed` and
   archive order drop them. `compile::system_libs` adds `-lm -lpthread` (Linux) and `-lgcc`
   (GNU) after the C++ standard library, as g++ and `verilated.mk` do.
2. Fresh builds were never cached (the coarse file clock, see "Inputs edited during a
   run").
3. The doctor warned `CXX unset` under pixi (see doctor.md).
4. Cold-cache tests passed 2 minutes on 4 vCPUs (CPU-bound, not hung); `.config/nextest.toml`
   gives them 6.

Setup notes: the cloud image has no C compiler. Under `pixi run`, conda's `cc` links, so
the README path works without one; a bare `cargo xtask setup` outside pixi needs
`build-essential` (the Lima provision installs it). The pixi installer sends telemetry
unless `PIXI_NO_TELEMETRY=1` is set before it. Not measured: the rerun-path count on Linux,
where every libc header under `/usr/include` is a rerun path (GCC's `-v` lists it).

## Measurements (in-process tests, opt-level 0, ccache warm)
Full `cargo nextest run -p simjig-build`: 37 tests, ~5-7 s wall. The fixture-crate test
(nested `cargo run` of a user crate) takes ~5 s warm.

## Not done / open
- Model C++ is compiled at the profile's `OPT_LEVEL` (0 in dev), so dev-profile models
  are slow. Verilator recommends `-Os`/`-O2` for model code. Needs a decision
  (performance vs. debuggability); a `CppRecipe` field would be the natural place.
- `PATH` is not in `rerun-if-env-changed` (as in `cc`, to avoid spurious reruns); a
  different `verilator` on PATH is only picked up when something else reruns the script.
  `SIMJIG_VERILATOR`/`VERILATOR_ROOT` are tracked.
- `CONDA_PREFIX` is tracked (its `lib/` is searched for the FST libraries), and so are
  `CXX`/`CXXFLAGS`/`LDFLAGS`, which a pixi activation may set. A test run outside `pixi run`
  after a build inside it therefore reruns the build script (and may not find Verilator).
  Removing that needs the library dirs derived from the resolved Verilator.
- Not verified: GCC on macOS, a Homebrew prefix, filesystems with timestamps coarser than
  2 s (the stat rule's margin), `OUT_DIR` on another filesystem than the cache (the `tmp/`
  fallback), non-UTF-8 paths in Verilator's `.d` (listings are written lossily: a miss, not
  a wrong hit), and Cargo's unstable new build-dir layout.
- `deny.toml`: added `Unicode-3.0` (unicode-ident, via thiserror's proc macro).

## Known limitations
- A parameter named twice inside one `bind_iface_with_params` list is not reported.
- The `<triple>/` arm of `simjig_target_dir`'s layout fallback is not reached in ordinary
  Cargo builds, where a tagged directory is found first; caches are per triple.
