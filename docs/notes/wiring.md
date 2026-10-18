# wiring notes (`simjig_build::Dut::build` end to end)

Agent `wiring`, branch `w2/wiring`. Checked against Verilator 5.052 (pixi, osx-arm64).

## Pipeline (`Dut::build_in`, `src/dut.rs`, `src/pipeline.rs`)
1. Validate (name is a C identifier, sources, **top is now required**), locate Verilator.
2. Claim the prefix (`pipeline::claim`, see below).
3. Type IR (`pipeline::design_ir`): `svtypes::verilator::json_pass` with the same sources,
   `-I`, `-D`, `-G` and extra arguments (`--no-timing` always). With interface bindings,
   the two-pass flow from docs/notes/codegen.md: `emit_elab_top` → JSON pass on
   `simjig_elab_<dut>` → `lower_wrapper` → `emit_wrapper` → JSON pass on
   `simjig_wrap_<dut>`; no `-G` anywhere (DUT params go into `IfaceTop.params`), and the
   wrapper is the model top. The JSON pass's Verilator version must equal the located one.
4. Emitter → `emit::place`: `<name>.rs` into `OUT_DIR/simjig/`, `<prefix>_shim.cpp` into `OUT_DIR/simjig/<name>/`
   (written only when changed), `<prefix>.vlt` and the layout check content-addressed
   into the cache.
5. Layout check: `verilator -Wno-fatal --lint-only -Werror-USERFATAL --[no-]timing
   --top-module simjig_layout_<top> -I.. -D.. <sources> <layout.sv> <user args>`, no `-G`.
6. `verilator --cc` through the cache, with the wrapper and `.vlt` before the sources.
7. Header cross-check: `check_ports(header, Emitted::ports or ir_ports(&design))`.
8. Compile and link (`src/compile.rs`).

## The Emitter seam (`src/emit.rs`)
```rust
pub type Emitter = fn(&svtypes::ir::Design, &DutConfig) -> Result<Emitted, Error>;
pub struct Emitted { rust, shim_cpp, vlt: Option<String>,
                     layout_check: Option<SvModule>, ports: Option<Vec<HeaderPort>> }
pub const DEFAULT_EMITTER: Emitter = codegen_emitter;   // noop_emitter also exported
```
`Dut::emitter(f)` replaces it. A plain `fn`, not a trait: emitters need only their two
inputs, and a fn pointer keeps `Dut: Clone + Debug`. The wrapper is not an emitter output:
it must exist *before* the IR the emitter sees, so the pipeline owns it.

`DutConfig::probes` / `ifaces` are now `codegen::Probe` / `codegen::IfaceBinding`.
**Builder API change:** `probe(path, module, ProbeType, ProbeAccess)`; the IR has no
internal variables, so module and type cannot be derived. `bind_iface(port, "if.modport")`
splits the modport; `bind_iface_with(IfaceBinding)` takes parameters.

## Compile recipe (`src/compile.rs`, `src/runtime.rs`)
As docs/notes/abi.md, with these decisions:
- `cpp/*` is embedded with `include_str!` and extracted to `cache/runtime/<hash>/`, so it
  works from any source location; a unit test fails if a `cpp/` file is not embedded.
- Shared archive `simjig_verilated_<key>`: `verilated`, `verilated_threads`,
  `verilated_timing`, `simjig_rt` with **fixed** switches (`VM_TIMING=1`, the rest 0), so
  timing, FST and plain models link the same archive. Model TUs get their own `_classes.mk`
  switches. Runtime classes beyond the base (`verilated_fst_c`, `verilated_cov`) get one
  cached archive each, compiled with the model's switches.
- The `vl_*` hooks are defined once, in `simjig_rt.o` inside the shared archive (`nm` test
  `the_fatal_hooks_are_defined_once_in_the_shared_archive`), so Verilator's own fatals
  (spikes: misused `nextTimeSlot()`) reach them.
- FST: `-llz4 -lz`, searched in `LDFLAGS -L` dirs then `$CONDA_PREFIX/lib`, with
  `-Wl,-rpath,<dir>` (conda ships lz4 only as a dylib). `rustc-link-arg` reaches only the
  package whose build script calls simjig-build.
- `-O2` regardless of profile; `SIMJIG_CXX_OPT` ∈ {0,1,2,3,s,z,g} overrides. Tracked:
  `PATH`, `SIMJIG_CXX_OPT`, `CONDA_PREFIX`, `LDFLAGS` (plus the earlier set).

## Prefix uniqueness (`pipeline::claim`)
The spikes showed a duplicate `--prefix` links silently with one copy of 88 symbols.
- Per build script (process): a DUT name may be rebuilt only with an identical config
  (identity = SHA-256 of the config's `Debug`); the name also names `<name>.rs`.
- Across crates: `cache/prefixes/<prefix>` stores the owning identity; another identity
  with the same prefix (a 32-bit hash collision) is an error.
- User-supplied `verilator_input`s now count towards the prefix.

## Verification results
| Item | Test | Result |
|---|---|---|
| ABI flags on every TU, `-O2` first `-O` flag, `SIMJIG_CXX_OPT=1` | `recipe::*` (logging ccache stand-in) | pass |
| `--trace-fst` links lz4/zlib and writes a real FST | `fixture_crate::fst_tracing_links_and_writes_a_trace` | pass; binary loads `@rpath/liblz4.1.dylib`, `@rpath/libz.1.dylib` |
| `--timing` (C++20 coroutines, codegen shim with `kTiming = true`) | `fixture_crate::timing_model_schedules_its_own_events` | pass: `next_time_slot` = 5000 ps, 5 ticks at 50 ns |
| Two prefixes in one binary | `fixture_crate::two_models_share_one_binary` | pass; all 5 models link one `simjig_verilated_<key>` |
| End to end: generated Rust `IR_HASH`/`SIGNALS` equal the vtable; counter wraps at 4096 | `fixture_crate::counter_vtable_end_to_end` | pass |
| Interface-ported DUT through the generated wrapper | `emitter::an_interface_ported_dut_is_built_through_the_generated_wrapper` | pass |
| Probe made public by the `.vlt` | `emitter::generated_files_reach_out_dir_verilator_and_the_compiler` | pass |
| IR ports = header ports (`ports.sv`, `struct_pipe`, `wide`) | `emitter::ir_ports_match_the_header` | pass |

## Surprises
- **pixi's `CXXFLAGS` already contains `-O2`**, and `cc` puts env flags last, so in
  `pixi run` dev builds were never `-O0`, and `SIMJIG_CXX_OPT` cannot lower the level
  there (the env `-O2` wins). Outside pixi the setting is what applies.
- The codegen layout module has no `timeunit`; with a design that declares one
  (`timing_dut`) Verilator fails `TIMESCALEMOD`. Hence `-Wno-fatal` before
  `-Werror-USERFATAL`. Worth a `timeunit`-agnostic fix in codegen.
- `cc` 1.8.0 with the `parallel` feature prints `exit status: 0` per compile to the build
  script's stdout regardless of `cargo_debug(false)` (`parallel/command_runner.rs:38`).
  Harmless (Cargo ignores non-directive lines).
- A shared append-only log is not safe for parallel compiler wrappers: long lines
  interleave. The recipe tests log one file per process.

## Not verified / open
- Linux (link order, `-lstdc++`, lz4 from distro paths).
- `svtypes::json_pass` runs `verilator` from the process `PATH`, not `SIMJIG_VERILATOR`;
  mismatched versions are an error, but a different binary of the same version is not
  caught. svtypes should take the executable.
- Neither the JSON passes nor the layout check are cached: each build-script run costs
  1-3 extra Verilator runs (a `--json-only` of the counter takes ~0.07 s).
- The cross-crate prefix claim is check-then-write, not atomic: two colliding identities
  racing can both pass once.
- The fixture's `stub.rs` is copied from `tests/codegen_it/stub.rs` at test time, so the
  fixture crate does not build in place.
