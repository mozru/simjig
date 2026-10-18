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

## Measurements (in-process tests, opt-level 0, ccache warm)
Full `cargo nextest run -p simjig-build`: 37 tests, ~5-7 s wall. The fixture-crate test
(nested `cargo run` of a user crate) takes ~5 s warm.

## Not done / open
- Model C++ is compiled at the profile's `OPT_LEVEL` (0 in dev), so dev-profile models
  are slow. Verilator recommends `-Os`/`-O2` for model code. Needs a decision
  (performance vs. debuggability); a `CppRecipe` field would be the natural place.
- Not verified: `--trace-fst` linking (`-llz4 -lz` emitted per `verilated.mk`), `--timing`
  (C++20 coroutines), Linux, and two crates linking the same `simjig_verilated_<key>`
  archive into one binary (likely fine for archives; duplicate symbols would appear if two
  models with *different* runtime flags, e.g. one with coverage, link into one binary).
- `PATH` is not in `rerun-if-env-changed` (as in `cc`, to avoid spurious reruns); a
  different `verilator` on PATH is only picked up when something else reruns the script.
  `SIMJIG_VERILATOR`/`VERILATOR_ROOT` are tracked.
- Headers included by `CppRecipe` sources are tracked only via `include_dirs`
  (`rerun-if-changed` on a directory); `cc` reports no dependency info.
- `deny.toml`: added `Unicode-3.0` (unicode-ident, via thiserror's proc macro).
