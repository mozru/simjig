# Changelog

All notable changes to simjig. Constitution rule 4 requires an entry for every change to
the public API, the generated code or simulation semantics.

## Unreleased
- The Cargo target directory is found by one rule, in the new crate `simjig-target-dir`,
  for both simjig-build's model cache and simjig's run directories (it was two copies).
  One change for the cache: Cargo's `CACHEDIR.TAG` is looked for at most five levels
  above `OUT_DIR`, as far as Cargo's layout reaches, so a tag on some directory above an
  untagged target directory no longer moves the cache there. And one for both: an absolute
  `CARGO_TARGET_DIR` counts only when it holds the path, and the deeper of it and the
  tagged ancestor wins. The cache no longer goes in a `CARGO_TARGET_DIR` that does not hold
  `OUT_DIR` (`--target-dir` overrode it), and run directories go in an untagged
  `CARGO_TARGET_DIR` inside a tagged `target/` (the cache already did).
- **Behaviour change: eval elision is off by default** (correctness over performance).
  Verilator 5.052 also re-runs, on every `eval()`, logic fed by an input the design writes
  itself (its `ASSIGNIN` lint waived) or by a `$sampled` value; when that logic reads
  `$time`, prints, draws `$urandom` or calls DPI-C, a skipped eval differed from a C++
  loop, and no build output shows it. Every eval now runs unless the test opts in with
  `#[simjig::test(elide_evals)]` or `ModelOptions::with_elide_evals(true)` (new public
  field `ModelOptions::elide_evals`, `TestMeta::elide_evals`; the generated `TestMeta` of
  every test gains `elide_evals: false`, snapshots reviewed). Its rustdoc says exactly when
  it is safe. Opting in still evaluates every time for a model with a writable public
  variable, `--timing` or waves. The cost of the default: about 9% on the benchmark FIFO's
  async C flow (298 against 272 ns per cycle) (`docs/notes/perf.md`, "Eval elision off by default"); traces and their
  hashes are unchanged, since a skipped eval was recorded as one.
- Fix: the waves of a synchronous test (`#[simjig::test(sync)]` with `SIMJIG_WAVES` or
  waves on failure) showed each timestep with the next edge's clock and inputs, half a
  period early: `SyncClock` and `set` write into the model before the next timestep
  starts, which is when a timestep was dumped. Each timestep of a synchronous run is now
  dumped right after its eval. Async runs are unchanged.
- Fix: eval elision (an eval with nothing written since the last one is skipped) no longer
  applies to a model with a variable C++ may write: `public_flat_rw` (or `public`,
  `public_flat`) on a variable in the source or a `.vlt`, `--public-flat-rw`,
  `--public-depth`, `--public-params`, or a read-write probe. Verilator 5.052 runs the
  logic such a variable feeds on every `eval()`, written or not, so skipping the eval
  changed what that logic saw (`$time`) and did (`$display`, DPI-C calls); such a model is
  now evaluated every time, as a C++ loop does. simjig-build finds these variables in the
  model's scope table and compiles the shim with `SIMJIG_PUBLIC_RW=1`; the C ABI gains the
  feature bit `SIMJIG_FEATURE_PUBLIC_RW` (16). Models without such a variable are
  unchanged.
- Fix: a trigger on a Verilator model's input (`changed()`, `rising()`, `falling()`) fired
  half a period late, at the next write, when the design itself changed that input in an
  eval (possible once its `ASSIGNIN` lint is waived): the value-change walk after an eval
  was skipped when only inputs were watched. That walk now always runs.
- `#[simjig::test(sync)]` denies `unused_must_use` in the test's body: an async helper
  called there (`reset(dut);`) only made a future and dropped it, so it silently did
  nothing, with only rustc's warning to show it. It is now a compile error. Generated
  code: the user's function gains `#[deny(unused_must_use)]` (snapshot `expands_sync`,
  reviewed). A body that discards another `#[must_use]` value on purpose writes
  `let _ = ..;`.
- Synchronous tests: `clock(..).start()` panics at the caller's line (it named a line
  inside simjig), and a `TimeLimit` failure with no blocked task (always so in a
  synchronous test) no longer blames a listed task in its `help:` line.
- `SyncClock`: a second `SyncClock::new` in one synchronous test panics ("one clock per
  synchronous test"), and so do a `rise()` while the clock is high and a `fall()` while it
  is low (both were silently accepted: a second clock advanced the same time axis, two
  rises made no edge); each panic names the caller's line, through `tick()` and
  `ticks(n)` too. New: `SyncClock::cycle()`, the rising edges so far.
- The failure report of a synchronous test gives the cycle its `SyncClock` reached after
  the time (`t=3 us (cycle 300)`); `FailureReport` gains `cycle: Option<u64>`.
- Fix: the run directory (`<target>/simjig/runs/..`) was not written, and the report
  said "no target directory found", when the target directory had no `CACHEDIR.TAG` (a
  `target/` another tool created first). The test binary's target directory is now found
  as the build cache's is: the tag, else an absolute `CARGO_TARGET_DIR` holding the
  binary, else Cargo's `<target>/[<triple>/]<profile>/deps/` layout.
- The waves replay of a run without a trace hash (a synchronous test, `trace_hash = false`)
  reports `reproduced (same seed, same outcome at the same time; ..events were not
  compared)` instead of claiming the same events. `WavesReplay` gains the public field
  `events_compared`.
- simjig-build no longer forces frame pointers in the model's C++ when debug info is on
  (the `cc` crate's default): the platform's default, as `verilated.mk` builds. Each eval of
  a small design is about a tenth cheaper; `CXXFLAGS=-fno-omit-frame-pointer` brings them
  back (it comes later on the command line).
- simjig-build compiles a model that Verilator did not split for a parallel build
  (`VM_PARALLEL_BUILDS = 0` in its `_classes.mk`) as one translation unit including every
  generated class (`<prefix>__ALL.cpp`, next to the Verilator output), as `verilated.mk`
  does: each eval of a small design is a few percent cheaper. A split model is compiled
  file by file, as before.
- Synchronous tests: `#[simjig::test(sync)]` on a plain `fn` (also with `duts = [..]`,
  `init`, `timeout`) runs it through `Sim::run_sync`, with `simjig::SyncClock` (`tick()`:
  rise, eval, fall, eval; `rise()`, `fall()`, `ticks(n)`) as its clock. A `set` goes into the
  model at once and the next eval takes it; there are no tasks, triggers, regions or delta
  cycles (`spawn`, a kernel clock and an awaited trigger panic, naming the mode); no trace
  hash is kept. Seeds, `ctx.rng`, random init, the time limit and the failure report with
  its `repro:` line work as in async tests. Generated code: `run_sync_test` for a `sync`
  test (snapshot `expands_sync`); the macro's errors mention `sync`
  (`harness_ui/not_async.stderr`, `bad_timeout.stderr`, reviewed). `SyncClock::bind`
  makes `Bound`/`BoundMut` handles that read and write the model's storage directly (trait
  `Bind`). The C ABI gains `hold`, `release` and `eval_held` (appended; vtable size 216),
  which a lone model without waves or Rust DPI-C imports uses for a synchronous run. README:
  "Synchronous testbenches".
- `#[simjig::test(trace_hash = false)]`: the run keeps no trace hash (about 16% faster on
  the benchmark's FIFO testbench). Generated code: `TestMeta` gains `trace_hash` (every
  expansion snapshot of `simjig_macros::test_attr` shows `trace_hash: true`; reviewed);
  the unknown-argument error lists the new argument (`harness_ui/bad_timeout.stderr`).
  The outcome's and `summary.json`'s `trace_hash` are then null.
- Performance (no change to semantics, the trace hash or `Sim::stats`): the scheduler
  allocates nothing per timestep in steady state (`tests/alloc_steady.rs`); a model
  verilated without `--timing` and not writing waves is not evaluated when nothing was
  written since its last eval (the eval is still counted and recorded); value-change walks
  and empty regions are skipped when they could do nothing; the trace hash computes the
  same FNV-1a values with fewer dependent steps. The benchmark's full FIFO testbench runs
  2.5x faster (`docs/notes/perf.md`). README: a "Performance" section with the
  recommended release profile (`codegen-units = 1`).
- `#[simjig::test]`: the error for a context parameter after a DUT now suggests
  `(ctx: &simjig::TestCtx, dut: Dut)` (was `sim`), and the macro's docs, grammar and
  expansion name the context `ctx` throughout (`__simjig_ctx` in the generated closure;
  reviewed snapshots `simjig_macros::test_attr` and `harness_ui/bad_params.stderr`). The
  README states the parameter order once: the context, then the DUT, both optional.
- Sweep modules (generated code, reviewed snapshots `codegen_it::sweep`): the ports' own
  associated types are named before the types their structs' fields and bundles bring,
  so adding a field to a struct port can no longer rename a flat port's type. Renamed:
  a flat port whose type a struct port declared before it also has in a field (`dq`:
  `din`'s `A` is now `Din`, `bout`'s `B` now `Bout`; a bundle's flat `side` gets `Side`,
  not its struct field's `A`). In the other snapshots only the declaration order of the
  associated types changed. `examples/lib` and the README's sweeps are unaffected.
- Build-script reruns (`archive::toolchain_from`): the parent of the compiler's
  `-print-file-name=include` answer counts as a toolchain directory only when that
  `include` is in the compiler's `-v` search list, and a search-list directory with a
  `c++` component only when it is under the compiler's prefix or the SDK. A header in a
  user's `CPLUS_INCLUDE_PATH` (`$HOME/libs/c++/..`) now reruns the build script when it
  changes.
- `Dut::sources` recognizes a file already added under another path (`rtl/../rtl/a.sv`,
  a symlink) by its canonical path, and keeps the path as first named, so the model's
  cache key is unchanged for unchanged input. Its docs say that a file named both in a
  `.file_list` and in `.sources` is not deduplicated.
- README: a "Project layout" section with the recommended `Cargo.toml` (lint table),
  `build.rs`, `src/lib.rs` and test file, which `xtask`'s
  `the_readme_project_layout_is_the_quickstart` checks against `examples/quickstart`;
  "The three moments at a clock edge" (`rising()`, `.read_write()`, `.read_only()`, with
  their SystemVerilog regions and cocotb spellings); the "Coming from cocotb" table gains
  `.read_write()` and uses the prelude's short names. The sweep examples (and
  `examples/lib`'s `sync_fifo` and `hop_stage` tests), which drive the next input, use
  `rising().read_write()`; the lockstep example, a pure checker, uses
  `rising().read_only()`.
- `examples/lib` uses the lib-target layout too: `src/lib.rs` re-exports the prelude (and
  `In`, `Out`), includes every DUT and holds `assert_param_checks` (from
  `tests/common/mod.rs`, removed); each test file starts with `use
  simjig_examples_lib::*;`. Both examples list their own lint tables, which `xtask`'s
  `example_lints_mirror_the_workspace` keeps equal to the workspace's but for
  `unwrap_used`, `expect_used` and `missing_docs`.
- `examples/quickstart` uses the recommended lib-target layout: `src/lib.rs` has `pub use
  simjig::prelude::*;`, the `include_dut!`s and a shared `reset`, and each test file
  starts with `use quickstart::*;`. It depends on `simjig` only (`simjig::bits`, no
  `simjig-bits`), lists its own `[lints.*]` tables (the workspace's, with `unwrap_used`,
  `expect_used` and `missing_docs` allowed), and the flop test checks the edge's result
  with `dut.clk.rising().read_write().await`. `include_dut!` in a test file still works.
- `simjig::prelude`: one glob import (`use simjig::prelude::*;`) for what nearly every
  testbench uses: `clock`, `cycles`, `ns`/`us`/`ps`/`ms`, `spawn`, `first!`, `join!`,
  `with_timeout`, `Timer`, `ReadWrite`, `ReadOnly`, `TriggerExt`, `Queue`, `Event`,
  `TestCtx`, `Rng`, `tb_log!`, `Bit`, `Bits`, `Value`, `Vector`, `Packed`. It can be
  glob-imported next to a sweep module (`use <sweep>::*;`), whose `Value`, `Vector` and
  `Packed` are the same items (`examples/lib/tests/prelude.rs`).
- `.read_write()` and `.read_only()` on triggers (`TriggerExt`, sealed; new types
  `ThenRegion` and `Cycles`): `dut.clk.rising().read_write().await` awaits the edge, then
  the `ReadWrite` region of the same timestep (cocotb's `await RisingEdge(clk); await
  ReadWrite()`), and `.read_only()` the `ReadOnly` region. They exist on `rising`,
  `falling`, `changed` (functions and handle methods), `cycles` and `Timer`, not on
  `Event`, `Lock` or `Queue`. `cycles` now returns the named future `Cycles` instead of
  `impl Future<Output = ()>` (source-compatible). The `WriteInReadOnly` help names
  `.read_only()` and points to `.read_write()`. No scheduler change: the conformance
  goldens are unchanged.
- README "A parameter sweep": the test snippet uses the short names `use sync_fifo::*;`
  allows (`duts = [fifo_8x4::Dut, ..]`, `D: Ports`), as the long form warns
  (`unused_qualifications`) next to that import; `examples/lib`'s `sync_fifo` tests do
  the same. It documents `.sources` against `.param`, `i32` parameter consts
  (`.unsigned_abs()`), and why a bit vector converts from `u128` only (`u128::from` for
  `rng.below`).
- `Dut::sources` skips a path already added (it used to pass Verilator the file twice,
  defining its modules twice). Its docs and `Dut::param`'s say that `.sources` appends
  while `.param` replaces, with the pattern for one member's own RTL.
- Sweep modules (generated code): a `Ports`, bundle or fields trait method named like a
  blanket impl's (`into`, `from`, `try_into`, `try_from`, `to_owned`, `clone_into`,
  `borrow`, `borrow_mut`) gets `_` appended like `clone_`, as `x.into()` was ambiguous
  (E0034). The rename note now reads "a supertrait or a blanket impl has a method".
- Sweep modules (generated code, reviewed snapshots `codegen_it::sweep`): a fields
  trait's associated types are `Ports`' and the bundle traits' too, fixed by the bound
  (`type Beat: BeatFields<Data = Self::Din, Tag = Self::Tag>`), recursively through
  nested structs (`type Outer: OuterFields<Inner = Self::Inner>`), so a struct field and
  a port of one type in every member compare without a `where` clause
  (`dout == beat.data()`). `Ports` and bundle traits gain the associated types a struct
  field brings (`D::Tag`); existing names are unchanged. A fields trait's associated type
  is no longer named `Raw` (it clashed with `Packed::Raw`, E0221, and the sweep did not
  compile): a field `raw` of a differing type gets `Raw2`.
- `LoadError::NoRandomInit` also names a shim generated by an older simjig-build as a
  cause, and its help says to rebuild. Android targets link `-lm` without `-lpthread`
  (Bionic has no libpthread).
- `examples/lib` gains `hop_stage`, a sweep over the interface `tagged_if` whose `beat_t`
  has parameter-dependent field widths; its test body (`D::Beat::new`, `beat.tag()`,
  one `use hop_stage::*;`) is the README's fields-trait example, compile-checked with
  the other sweep snippets. The README sweep section shows it before and after.
- Sweep modules (generated code, reviewed snapshots `codegen_it::sweep`): a generic body
  needs one import, `use <sweep>::*;`, which now also brings `Value`, `Vector` and
  `Packed` (re-exported unless a shared type has the name). A bundle trait's associated
  types are `Ports`' too, fixed by the bound (`type Up: BeatIfSinkPorts<Beat =
  Self::Beat>`), so the same type on two interface ports (`up.beat`, `down.beat`) is one
  type, `D::Beat`. A packed struct that is an associated type gets a fields trait
  (`BeatFields: Value`, the bound of `D::Beat`): getters, `new(data, tag)` and
  `with_<field>`, taking anything that converts into the field (`D::Beat::new(0x5a, 1)`).
  A trait method named like a supertrait's (`clone`, `fmt`; for a fields trait also
  `eq`, `to_raw`, `new`, ...) gets `_` appended, with a note in its docs.
- Generated code: a struct typedef'd inside an interface (`beat_if`'s `beat_t`) is named
  after its typedef (`Beat`) instead of the first wrapper port that carries it
  (`UpBeat`). The wrapper spells such a type out, so it reached the IR anonymous. No
  existing snapshot changes; `codegen_it::snapshots::an_interface_typedef_names_*`.
- A malformed parameter value names its call in the error's headline:
  ``elaborating DUT `acc` (its `.param("W", ..)` = `3#`) failed ...``, for a `-G`
  override reported at the parameter's declaration and for a value in the generated
  elaboration top (`.param` or `.bind_iface_with_params`). The interface-default `hint:`
  line now starts with the DUT (``DUT `gb`: the width error may come from ...``), so a
  sweep member's hint says which member.
- A model's build script no longer prints `cargo::rerun-if-changed` for the toolchain's
  own headers: the compiler's resource directory (`-print-resource-dir`, GCC's
  `-print-file-name=include`), its C++ library (the search list's `c++/v1`, `c++/<ver>`)
  and the SDK (`xcrun --show-sdk-path`, `-isysroot`). They change only with the
  compiler, whose identity keys the archives and whose executable the script reruns on;
  the archive keys still hash them. Other headers of the search list (a conda or
  Homebrew prefix's `include/`, `/usr/local/include`, `/usr/include` on Linux) still
  rerun it, so a library upgrade does. This was most of a failed build's `--- stdout`
  (`simjig`'s own test build script on macOS: 1,268 distinct paths before, 69 after,
  three of them the conda prefix's `zlib.h`, `zconf.h` and `lz4.h`).
- `Dut::param` set twice for one parameter keeps the last value (in the first call's
  place) instead of passing Verilator two `-G` options ("Duplicate parameter
  connection"), so a sweep member can override a base configuration. A second
  `bind_iface*` of one port is now a configuration error naming both calls; it used to
  make the wrapper instantiate two interfaces.
- Parameter sweeps are first class. `simjig_build::Sweep::new("sync_fifo").dut(a).dut(b)
  .build()` builds several configurations of one module and one Rust module over them:
  `simjig::include_dut!(sync_fifo)` brings in each configuration
  (`sync_fifo::fifo_8x4::Dut`), the generated types equal in every configuration,
  defined once (`fifo_8x4::Item` is `sync_fifo::Item`), and the trait
  `sync_fifo::Ports`, which every configuration's `Dut` implements: one method per
  signal (`dut.clk()`), the parameters as associated consts (`D::DEPTH`), and an
  associated type where the configurations' types differ (`D::Count`, shared by signals
  that agree in every configuration), bounded by the new `simjig::Vector` or
  `simjig::Value`. An interface port whose bundle differs gets a bundle trait. The README
  trait-and-macro pattern is gone; see "A parameter sweep". A sweep's member files import
  the shared types from the sweep module (`pub use super::{..}`), so they are included
  through it; a DUT built outside a sweep is generated as before (snapshots unchanged).
  Reviewed new snapshots: `codegen_it::sweep` (`sweep__*`, `iface_sweep__*`).
- `#[simjig::test(duts = [a::Dut, b::Dut])]` on a generic `async fn t<D: s::Ports>(dut: D)`
  makes one test per listed type (`t::a`, `t::b`), each running the body as written, so a
  failure points into it. The messages for a generic test and an unknown argument now
  mention `duts`.
- `simjig::Value` (a value a handle gets and sets, compares and prints; `from_u128` and
  `to_u128` reach its packed bits, an escape hatch for an opaque associated type) and
  `simjig::Vector` (a bit vector: from and `==` a `u128`), implemented for every type
  with their bounds.
- A DUT may not be named like a sweep of the same build script (both name
  `OUT_DIR/simjig/<name>.rs`), and a DUT may be in one sweep only.
- `examples/lib` builds `sync_fifo` as the sweep of `fifo_8x4` and `fifo_32x16`; its raw
  FIFO tests run on both (`random_traffic_matches_the_model::fifo_32x16`).
- Random initial values, to catch a register that reset never clears:
  `#[simjig::test(init = random)]` (or `init = zero`), or `SIMJIG_INIT=random` for tests
  without the argument. Each model draws from its own seed, `Seed::init_seed(index)`
  (stream `init.model-<index>`), except that in a multi-DUT test the models of one
  design (`DutModel::design`, the top module; new, default `None`) share the first one's,
  so a `Lockstep` pair starts alike; `$urandom` is unchanged. New public API:
  `simjig::InitValues`, `ModelOptions::{init, init_seed, with_init, random_init}`,
  `RunIdentity::init`, `RunEnv::init`, `TestMeta::init`, `LoadError::NoRandomInit`,
  `simjig_build::XInitial` and `Dut::x_initial` (default `Unique`, which passes no flag).
  A random run's report header says `init=random`, its repro starts with
  `SIMJIG_INIT=random`, and `summary.json` gains `init` and `init_seed` (also written,
  as `"zero"` and `null`, for zero runs). Zero runs, default builds, prefixes and cache keys
  are unchanged. Breaking: a raw `--x-initial` in `verilator_args` or an option file is
  refused (use `.x_initial(..)`); `TestMeta`, `RunEnv`, `RunIdentity`, `ShimOptions` and
  `DutConfig` have a new field, and `LoadError` a new variant. Reviewed snapshot diff: every
  `#[simjig::test]` expansion gains `init: None` (`simjig-macros` snapshots).
- C ABI (version 1, append-only): `simjig_create_opts` gains `init` and `init_seed`
  (size 32 to 40; a caller passing size 32 gets zeros), `SIMJIG_INIT_ZERO`/`SIMJIG_INIT_RANDOM`
  and the feature bit `SIMJIG_FEATURE_RANDOM_INIT` (8), set unless the shim's traits say
  `kXInitialUnique = false`. The vtable (size 192) is unchanged; generated shims change only
  for a non-default `XInitial`. Every default model's `features` now has bit 8.
- Sweeps and random initial values together: `#[simjig::test(duts = [..], init = random)]`
  gives every per-DUT test the one `init`, and each sweep member honours its own
  `Dut::x_initial`, which is part of its model digest (cache key), like every other
  `DutConfig` field. The `duts` snapshots gain `init: None`, as every expansion does.
- A failed `Dut::build` prints its cause first, as `cargo::warning=simjig: ..` lines: its
  hints, its headline and, when the headline cannot carry it (a fatal `%Warning-` under
  the default lint), that diagnostic. Cargo shows the warnings above its dump of a failed
  script's stdout, so the cause now leads the output as well as ending it. A script that
  handles the error gets the same warning: one that also warns the error's first line,
  as the README's optional-DUT example used to, now warns it twice and can drop it.
- A build that stops on a width diagnostic (`%Warning-WIDTHTRUNC` under the default
  fatal lint, or an `-Werror-WIDTH*` in the JSON pass) of a DUT whose module and a bound
  interface default a shared parameter to different values starts its error with
  ``hint: the width error may come from interface `out_if` (bound to `out`) defaulting
  `W_OUT` to 8 while `gearbox` defaults it to 16; ..``. Before, the interface-parameter
  warning was printed only by a build that succeeded.
- The interface-parameter warning and hint also name the change-free way to silence
  them: binding the interface's own value, `.bind_iface_with_params(.., [("W_OUT", 8)])`.
- A failed `Sweep::build` warns its cause first too: the sweep's own error (no members,
  its name taken, members of two modules) as `cargo::warning=simjig: ..` lines, a
  member's error once, by the member's `Dut::build`.
- `Error::Command` has a new field, `hints`: what the configuration suggests about the
  failure, printed first, one `hint:` line each (breaking for a pattern or literal that
  names every field).
- Linux: models link. The build script now also emits `-lm`, `-lpthread` and (GNU
  targets) `-lgcc` after the C++ standard library; before, every test that linked a
  model failed on Linux with `undefined reference to symbol 'exp2@@GLIBC_2.17'`, and on
  aarch64 then with `undefined reference to '__aarch64_ldadd4_acq_rel'`. No change on
  macOS.
- Linux: a fresh build caches its Verilator and C++ outputs. Its start is now taken one
  tick of the filesystem's clock after the work directory, so an input written just
  before it (the generated `.vlt`) no longer has the same coarse time and is no longer
  taken as changed during the run (`changed while verilating (or its time is in the
  future)` for every DUT, and verilating again on the next build). On macOS the start is
  unchanged.
- `--threads` out of `int` range is read as 5.052 reads it, `(int)strtol(..)`:
  `--threads 4294967297` is one thread and passes; `--threads 4294967298` is two and is
  refused; `--threads 2147483648` is negative and left to Verilator's own error.
- A NUL byte in an option file ends its line, as in 5.052, which walks each line as a C
  string: the checks no longer see (and refuse) words after it.
- A `Starved` failure's help names the way out for a combinational design: ``start a
  clock; for a combinational design, await a Timer after each set
  (`Timer::new(ns(1)).await`); ..``.
- `cargo simjig doctor --help` (and `-h`, `help doctor`) prints the doctor's own help
  and exits 0, like `waves` and `show`; `cargo simjig help <command>` prints one
  command's help. Help that was asked for now goes to stdout (it went to stderr); a
  usage error still goes to stderr with exit status 2.
- The hint for a build that failed on lint warnings recommends waiving the one code
  (`-Wno-<CODE>`, `lint_off`) first, and says that `-Wno-fatal` also hides width
  mismatches between an interface and the module bound to it.
- The dashless `no` forms of on/off options are read like the dashed ones, as 5.052's
  option parser does: `--notiming` and `--notrace` are refused like `--no-timing` and
  `--no-trace`, and `--nocoverage-line` turns line coverage off in
  `DutConfig::features`.
- The generated `Dut` docs of an interface-ported DUT name the user's module
  (`` `iface_top.clk`: input, 1 bit ``), not the generated wrapper
  (`simjig_wrap_iface_top.clk`). Docs only: the signal names the runtime reports are
  unchanged. Reviewed snapshot diff: `iface_top__rust`, `producer__rust`.
- An option refused in a file list or option file ends with the way out for a vendor
  list that is not the user's to edit: ``copy it, delete line N from the copy and build
  from the copy``.
- A Verilator failure whose first error has no location names the offending file in the
  headline (``failed (exit status: 1) on `/r/d.sv`: ..``), as before the headline
  change; a first error on the last line of stderr, without a newline, is no longer
  printed twice; and a command line whose `-Mdir` was a temporary directory (the JSON
  pass's, the cache's work directory) says it was removed after the run.
- A DUT name claimed by an earlier DUT that failed reads as ``already claimed by an
  earlier DUT (which failed)``, not ``already built``, when a corrected configuration
  of the same name (or a name differing only in case) is refused.
- A `.param` that a bound interface also declares (`WIDTH` of `skid_buffer` and of its
  `stream_if` ports) is that interface parameter's default: `.param("WIDTH", 16)` with
  `.bind_iface("up", "stream_if.sink")` now gives the interface `WIDTH` 16, not the
  interface's own default. A value the binding sets
  (`.bind_iface_with_params("out", "s_if.source", [("W", 16)])`) always wins, also when
  it differs from the `.param`: interface port headers carry no parameters (IEEE
  1800-2017 §25.5), so the shared name is only a heuristic, and a module may read a
  wider interface through its port. This changes the generated wrapper of a DUT that
  relied on the interface's default.
- A build warns when the DUT module and a bound interface both declare a parameter,
  neither the binding nor a `.param` sets it, and their values differ (`up16`'s `WIDTH`
  defaults to 16, `bus_if`'s to 8): ``simjig: DUT `up16_unset`: module `up16` has
  `WIDTH` = 16 but interface `bus_if` (bound to `up`) has `WIDTH` = 8, and neither is
  set; ..``, naming the `.bind_iface_with_params(..)` that settles it. Under
  `-Wno-fatal` such a DUT built silently with a port of the wrong width. Setting either
  side explicitly silences it.
- A misspelled interface parameter in `bind_iface_with_params("up", "acc_if.sink",
  [("WW", 12)])` fails like a misspelled `.param`: ``interface `acc_if` (bound to `up`)
  has no parameter `WW`; did you mean `W`? (it has W)``, instead of Verilator's
  `PINNOTFOUND` in the hashed generated top. Overriding a localparam, the module's or an
  interface's, says ``module `acc` has no parameter `L`: `L` is a localparam and cannot
  be overridden``.
- `--threads N` is refused only when Verilator would run more than one thread: it
  reads `N` with C's `atoi`, so `--threads 01` (and the deprecated `0`) are one thread
  and pass. `--prefix=Vx`, `--top-module=x` and the other `=` forms are no longer
  refused as if they were `--prefix Vx`: 5.052 has no `=` forms and reports them as an
  `Invalid option` itself.
- Option files (`-f`, `-F`, file lists) are read as bytes, as Verilator 5.052 reads
  them: a file that is not UTF-8 (a Latin-1 `// Author: J\xf6rg`) was skipped whole by
  the configuration checks, so its `--top-module` silently built another top. A CRLF
  line keeps its `\r` after a `\` or inside quotes, and whitespace is C's ASCII
  `isspace` (`\v` and `\f` split words; a no-break space does not). A path in an option
  file that is not UTF-8 is refused, naming the line: Cargo's `rerun-if-changed` cannot
  name it.
- A DUT claims its name before its configuration is checked, so one that fails
  `validate` (or locating Verilator) still owns `OUT_DIR/simjig/<name>.rs`: a later DUT
  whose name differs only in case (`Counter` after `counter`) is refused instead of
  overwriting the failure stub on a case-insensitive filesystem.
- New `Dut::bind_iface_with_params(port, "iface.modport", [("WIDTH", 8)])`: an
  interface binding with parameter overrides whose values are written like `.param`'s
  (`Display`), instead of an `IfaceBinding` literal of strings. Additive.
- A Verilator failure prints its first error once, as the headline with the DUT and
  the location (``elaborating DUT `d` failed (exit status: 1) at /r/d.sv:3:5: syntax
  error``), then the rest of the excerpt, a hint, and the command; the `%Error` line
  was printed in the headline and again under `stderr:`. The JSON pass reads
  ``elaborating DUT `d` `` (its command line still shows `--json-only`), not
  ``parsing DUT `d` (Verilator JSON pass)``. A type, package scope or name Verilator
  cannot find (`Can't find typedef`, `Package/class for .. not found`, `before
  declaration`) hints at the package's import and its place before its users.
- A `.param` naming no parameter of the top fails with
  ``module `rate_limiter` has no parameter `PERIDO`; did you mean `PERIOD`? (it has
  BURST, PERIOD)``, instead of Verilator's `-G` message or a `PINNOTFOUND` located in
  the hashed generated wrapper. The parameters come from a second JSON pass without the
  overrides, run only on that failure.
- A file list (`Dut::file_list`, `file_list_relative`) or an `-f`/`-F` file among
  `verilator_args` may not carry an option the builder refuses as an argument
  (`--top-module`, `--timing`, trace switches, `--threads N`, `--prefix`, `--Mdir`,
  `--cc`/`--exe`/`--build`, ...): Verilator reads the list after simjig's own options and
  takes the last value, so `--top-module` in a vendor list silently built another top.
  The error names the file, the line and the builder call to use. Only `-G` was refused.
- Option files (`-f`, `-F`, file lists) are split into words as Verilator 5.052 does:
  `\` escapes, `"..."` and `'"..."'` quotes, and `// verilator fargs` comments, which
  replace the file's words. The configuration checks and a failed build's reruns read
  the same words Verilator does (a quoted `"-GX=1"` was seen with its quotes dropped
  only at the ends, an escaped space split the word). The `-G` refusal names the line.
- A successful `Dut::build` tracks a `CppRecipe::include_dirs` entry by the files
  directly in it, never an ancestor of `OUT_DIR`, as a failed build does: an include
  directory of `.` is the package directory, which Cargo scanned whole, `target/`
  included, so the build script reran on every build.
- README: "A parameter sweep", writing each test once against a user trait
  implemented for each configuration's `Dut`.
- `simjig::now_display()` prints the current time in physical units without the
  precision (`now().display(ctx.precision())` needed `ctx`), and `simjig::tb_log!(..)`
  (not `log!`, which a glob import of the `log` crate's `log!` would make ambiguous)
  prints a `format!` line prefixed with it and the running task:
  `[1250 ns main.monitor] hdr len=3`. The `first!` docs and the cocotb table show how to
  tell which operand won (an enum of your own).
- Generated code: the doc comment of an interface-ported DUT's parameter constant names
  the user's module (`/// Parameter `N` of `pkt_parser`.`), not the generated wrapper
  `simjig_wrap_<top>`, which has no parameters. No snapshot covered it.
- A successful build whose emitter writes no Rust (`Emitted::rust: None`) removes
  `OUT_DIR/simjig/<name>.rs`: a failure stub (or module) an earlier build left survived.
- A build script refuses a second DUT whose name differs from an earlier one's only in
  case (`counter`, `Counter`): on a case-insensitive filesystem both are
  `OUT_DIR/simjig/counter.rs`, so the second overwrote the first's module (or its
  failure stub did).
- A Verilator failure no longer buries its `%Error` under warnings: the first line of
  the message ends with the first error (`... failed (exit status: 1) on `x.sv`:
  %Error: x.sv:3:5: ...`), so a build script printing that line for a skipped DUT shows
  it; a long stderr is shown as every `%Error` line, then its first 40 lines; and the
  JSON pass's error drops the warnings located in simjig's generated tops (under
  `-Wall`, one `PINMISSING` per data port the elaboration top leaves open). The lint
  waiver hint is given only when no error is reported: a truncated
  `Exiting due to 1 error(s), N warning(s)` counted as warnings only.
- `Dut::file_list(path)` (`-f`) and `Dut::file_list_relative(path)` (`-F`) add a
  Verilator file list whose files are the DUT's sources: read in order before
  `.sources`, by the JSON passes, the layout check and the model build, and part of the
  prefix, the cache keys (the list's contents and, through Verilator's dependencies,
  the files in it) and the failed-build reruns. A DUT may have file lists and no
  `.sources`; a missing list fails the configuration check. New public API:
  `DutConfig::file_lists`, `svtypes::verilator::FileList` and
  `JsonPassOpts::file_lists` (passed after `-G`, before the sources), all additive but
  breaking for a struct literal without `..`. A DUT with no sources whose `-f` is among
  `verilator_args` is told to use `.file_list`.
- A failed `Dut::build` tracks a directory it names (`+incdir+`, `-I`, `-y`,
  `.include_dirs`) by the files directly in it, and never an ancestor of `OUT_DIR`: Cargo
  scans a tracked directory recursively, so a handled DUT naming the package directory
  (`+incdir+.`) reran the build script on every build, through `target/`.
- A failed `Dut::build` resolves the paths inside an `-f` file named in an `-F` file
  against the working directory, as Verilator 5.052 does; they were joined to the `-F`
  file's directory, so its reruns (and the `-G` check) looked at the wrong files.
- A Verilator failure on `TIMESCALEMOD` hints at the units missing a `timeunit`
  (`` `file:line` ``) and `.verilator_args(["--timescale", "1ns/1ps"])`, instead of the
  generic lint waiver.
- `Elapsed`'s `Debug` (what `.expect()` on a timeout prints) shows physical units,
  `Elapsed { after: 3 ns, at: 5 ns, precision: 1ns }`; it printed raw tick counts.
  `SimTime` and `SimDuration` keep their tick `Debug`: they carry no precision.
- A failed `Dut::build` also reruns on the paths inside its `-f`/`-F` files and in its
  `verilator_args`: sources, `-v` files, `-y`, `-I` and `+incdir+` directories, resolved
  as Verilator does (an `-F` file's relative to its directory). A handled DUT whose
  sources came from a file list or library directory never rebuilt after a fix.
- A failed `Dut::build` (or `build_in`) replaces `OUT_DIR/simjig/<name>.rs` with a stub
  whose expansion is `compile_error!("DUT `<name>` failed to build ...")`: a module left
  by an earlier good build (or written before a later step failed) compiled, and the
  test then failed to link (`Undefined symbols: _simjig_vtable_V<dut>_...`). A built
  DUT sets the `cfg` `simjig_dut_<name>` (new directives `cargo::rustc-cfg` and
  `cargo::rustc-check-cfg`; a failed one prints only the `rustc-check-cfg`), the gate for
  an optional DUT: `#[cfg(simjig_dut_<name>)]`.
- The generated wrapper of a DUT with interface ports (`simjig_wrap_<dut>`), and its
  elaboration-only top, waive `TIMESCALEMOD` for themselves (`// verilator lint_off`
  around the module) and take the design's time unit: a design whose units all declare
  a `timeunit` stopped on `TIMESCALEMOD` without `--timescale`. Generated SystemVerilog:
  two comment lines more (snapshots updated).
- `with_timeout(ns(0), ..)` panics with "with_timeout of zero ticks", pointing at the
  `with_timeout` call; it said "Timer of zero ticks" at a line inside simjig. It is a
  plain `fn` returning the future now (`#[track_caller]`), with the same output type.
- The `FailureKind::Timeout` failure of `with_timeout(..).await?` points at the `?`
  (`Failure::location`); it had none, so every timeout of a test shared one signature.
- A `Setup` failure for a `Task` handle dropped before its task ran names the task
  whose poll dropped it even when that task finished in the same poll (a detached
  child's last poll); it named `<finished task>`.
- `cargo simjig show` prints the file's path relative to the current directory, and the
  module with `::simjig::__private::` and `::core::primitive::` removed (`In<Bit>`, `u8`),
  saying so in its first line; `--path` still prints the absolute path.
- A failed JSON pass reads "parsing DUT `x` (Verilator JSON pass) failed ..." (was
  "JSON pass for DUT `x` failed ...").
- `cycles(clk, n)` takes any integer count (`n: impl TryInto<u64>`: `u32`, `usize`,
  `u64`, and an unsuffixed literal, which falls back to `i32`); a negative count panics
  at the call. It is a plain `fn` returning the future now, so the panic points there.
- `Queue::new(name, 0)` is an unbounded queue (`put` never waits), as cocotb's
  `Queue(maxsize=0)`; it panicked. Breaking: `Queue::capacity` returns
  `Option<NonZeroUsize>`, `None` for an unbounded queue (was a `usize`, 0 for it). A large
  capacity (`usize::MAX`) no longer reserves its whole size up front, which panicked.
- `with_timeout(..).await?` works in a test returning `simjig::Result`: `Failure`
  implements `From<Elapsed>`, as the new `FailureKind::Timeout { after }`, a failure of
  the awaiting task at the time the timeout elapsed. Breaking: `Elapsed` is
  `#[non_exhaustive]` and also carries `at` and `precision`, and its `Display` prints the
  timeout in physical units (`timed out after 400 us`, was `400000000 ticks`).
- A `Task` handle dropped before its task ran no longer masks the test's own error:
  dropped in the poll in which `main` returns (an early `return`, or `?` with the test's
  error, including a `run_from_checkpoint` refusal), it no longer fails the run, which
  ends with `main`'s result. Anywhere else it still fails as `Setup`, now when the poll
  ends, naming every handle that poll dropped (with its `spawn` line), saying it was
  dropped by `let _ = spawn(..)` or by leaving the scope early, and naming the fixes.
- Breaking: `simjig_build::CacheStatus` and `simjig::SeedSource` are `#[non_exhaustive]`,
  so a `match` on them outside their crate needs a wildcard arm.
- A model archive compiled from a verilation kept out of the cache (an input edited while
  Verilator read it) is kept out too, in `OUT_DIR/simjig-unpublished/`: its key named
  that one-off output, so the entry was never found again. A later good build of the
  DUT removes the model's outputs kept there.
- The shared cache is found in a target directory without Cargo's `CACHEDIR.TAG` too, by
  `OUT_DIR`'s layout (`<target>/[<triple>/]<profile>/build/<pkg>-<hash>/out`): a target
  directory another tool created first (`--target-dir target/rust-analyzer` creates
  `target/`) has no tag, and every crate fell back to its own `OUT_DIR/simjig-cache`. A
  build that still falls back warns (`cargo::warning`).
- `cargo simjig show <dut> [-p <package>] [--path]` prints the module that
  `include_dut!(<dut>)` includes, or its path under `target/`. The README has a section
  on the generated names (suffixes, enum prefixes, keywords, collisions).
- An integer literal compares equal to a `Bits<N>`, `WideBits` or `Bit` from the left too
  (`PartialEq<Bits<N>> for u128`), so `assert_eq!(4, x.get())` compiles. Only `u128`:
  an impl for another integer type (`u32`, `usize`, ...) or `bool` makes
  `n == "3".parse().unwrap()` and `assert_eq!(total, xs.iter().sum())` ambiguous (E0283)
  in every crate linking simjig, so a typed `n_u8 == x` is written `x == u128::from(n_u8)`.
- A model that fails to verilate only because of lint warnings gets a `hint:` naming the
  waivers: a `lint_off` comment, `.verilator_args(["-Wno-<CODE>"])`, or
  `.verilator_args(["-Wno-fatal"])`.
- A failed `Dut::build` prints none of that DUT's build directives, and no
  `cargo::error=` (which fails the build even when the script handles the `Err` and exits
  0), and returns the error. It prints only `rerun-if-changed` for the inputs the
  configuration names (sources, include directories, `-f`/`-F` option files, the C++
  recipe's files) and `rerun-if-env-changed` for the tracked variables, so a script that
  handles the error reruns when that DUT is fixed. With `?` from `main` or `unwrap`, the error is on stderr,
  which Cargo shows last, after its dump of the script's stdout.
- Breaking: dropping a `Task` handle before its task ever ran, without awaiting it, fails
  the run (`FailureKind::Setup`, at the `spawn` call, naming the task and the fixes:
  `.detach()`, `.await`, `.cancel()`). It printed a warning that test runners capture, so
  `let _ = spawn(..)`, which rustc's `must_use` help suggests, was a test that passed
  without its driver. Cancel such a task with `Task::cancel`. A combinator dropping a
  losing operand, a cancelled task's locals and the end of the run are unaffected; so is
  dropping a handle whose task has run. The `must_use` message of `Task` says the same.
- IR schema 5: no new fields; it marks the DPI fields stage 3 added under schema 4
  (`Design::dpi_imports`, `Design::dpi_exports`), so schema 4 no longer has two JSON forms
  and a front-cache entry from a build without them is not served. Every `IR_HASH` (and
  so every generated binding and shim) changes, and every DUT's JSON pass runs once more.
- `Dut::dpi_in_c` is no longer part of a DUT's identity: moving a DPI-C import from Rust
  to C (it changes only the generated Rust, not the model) failed the next build with
  "model prefix ... is already used by a different configuration; rename the DUT" until
  the cache was cleared. A prefix claim now records the full digest the prefix is cut
  from (`DutConfig::model_digest`), and the prefix hashes probes and interface bindings
  field by field instead of through their `Debug` form (`simjig-prefix-v3`), so every
  model prefix changes and every model is verilated and compiled again once. Within one
  build script, a DUT name rebuilt with another `dpi_in_c` is still refused.
- A DUT's model prefix is claimed in the cache after the front end's checks (the JSON
  pass, `dpi_in_c`, the emitters, the layout check), so a build that fails them leaves no
  claim behind.
- `fork::run_from_checkpoint` inside an operand of a `join!`, `first!` or `with_timeout`
  of the main task returns the new `ForkError::InCombinator`. It forked: the other
  operands ran on inside every branch, and an operand that won a `first!` in the child
  cancelled the branch body, so the child ran the rest of the test as if it were the
  parent.
- A fork branch's child whose `run_from_checkpoint` future is dropped before the branch
  body returns (by a hand-written poll) ends with a failed branch ("the branch body was
  dropped before it returned"). It ran on as if it were the parent.
- Breaking: the runtime's enums that may grow are `#[non_exhaustive]`, so a `match` on them
  outside `simjig` needs a wildcard arm: `simjig::{FailureKind, TraceKind, ClockError,
  TimeError}`.
- The shim is stored content-addressed in the cache (`inputs/<hash>/<prefix>_shim.cpp`)
  and compiled from there; it is no longer written to `OUT_DIR/simjig/`, which holds only
  the generated Rust. A model archive's key has the shim's path, so crates and target
  directories that build the same configuration now share one model archive (each
  compiled its own), and it no longer lists the files in `OUT_DIR/simjig/` or in the
  cache, so a rerun of the build script compiles only the models whose DUT changed (it
  compiled every model after any DUT changed). Every model archive is compiled again once.
- `create` refuses a probe of a `real`, a `string` or another variable that is not a
  packed vector by its type, with the build-time check's words ("`r` is a `real`
  (Verilator stores it as VLVT_REAL) ..."), before comparing widths: a 1-bit probe of a
  `real` (which Verilator records as 1 bit wide) got "not stored as a packed vector".
- A parameter override (`-G`, `-pvalue+`) inside an option file (`-f`, `-F`, nested ones
  too) among the extra Verilator arguments is refused like one among the arguments, with
  the `.param(name, value)` hint. It failed later, in the layout check.
- An input edited while Verilator, the JSON pass or the C++ compiler read it no longer
  poisons the cache. Each run snapshots its start (and the digests of the inputs a
  previous run listed); when a file it read has a modification or change time not
  before the start, or another digest, its output serves that build only (from
  `OUT_DIR/simjig-unpublished/`), the build warns (`cargo::warning`), and the next build
  runs again. Before, the output of the old contents was cached under the new contents'
  key and hit on every later build. Verilation manifests now record digests and file
  status like the archive ones: every model is verilated again once
  (`simjig-verilate-deps-v2`).
- The build script reruns when the C++ compiler changes in place: it emits
  `cargo::rerun-if-changed` for the path `CXX` (or `cc`'s default) is found at, the
  executable that resolves to, and the same for a compiler wrapper (`CXX="ccache c++"`).
- Archive manifests (`manifests/<pre key>`) record each dependency's digest and file
  status; a later build reads and hashes again only the files whose status (size,
  modification and change times, inode, device) changed. Every archive is compiled again
  once (`simjig-archive-deps-v2`).
- The type IR of each DUT and the layout check's verdict are cached (`fronts/`,
  `checks/` in the cache), keyed on the JSON pass's arguments and inputs and on every
  file the model's verilation read: a rerun that changes nothing runs no JSON pass and
  no layout check. A build script also locates Verilator, sets the C++ compiler up and
  looks the Verilator runtime up once instead of once per DUT.
- The build-time probe check reads each variable's `VLVT_*` type from the scope table: a
  probe of a `real`, a `string` or another variable that is not stored as a packed vector
  fails the build naming its type (it passed as one bit, and `create` refused it), and so
  does a variable whose storage is not the one its width has.
- Breaking: the public enums that may grow are `#[non_exhaustive]`, so a `match` on them
  outside their crate needs a wildcard arm: `svtypes::ir::{TypeKind, VariableKind,
  ParamValue, ModuleKind}`, `simjig_build::codegen::{ProbeType, SignalTarget, ItemKind,
  RustType, ConstValue, DutField}`, `simjig_build::PortProblem` and
  `cargo_simjig::doctor::{Check, Status, Format}`. The lowering reports an IR kind it
  does not know as `codegen::Error::Unsupported`.
- A probe path that ends at a generate-for iteration or an instance array element
  (`top.g[0]`, `top.arr[1]`) says that it names a generate block or an instance array,
  instead of "not a plain identifier".
- A probe whose scope or variable Verilator did not make public gets the same diagnosis
  at build time and at `create`: "no public scope `s` in the model: Verilator made nothing
  in it public", or "no public variable `v` in scope `s` (a probe inside a hierarchical
  block, `hier_block`, is in another model)". The build-time check gave the
  hierarchical-block reason for a missing scope, where it cannot apply.
- `CppRecipe::flags` reach the model's translation units only, no longer the shared
  Verilator runtime: a DUT's flags compiled a libverilated of its own, and two in one
  binary define the same symbols. Flags that must reach every translation unit (ABI
  switches such as `-D_GLIBCXX_DEBUG`, `-stdlib=`, `-fsanitize=`) belong in `CXXFLAGS`.
- Initial workspace:
  - `svtypes`: the IR from Verilator `--json-only`;
  - `simjig-bits`: bit-vector values;
  - `simjig-build`: the pipeline, codegen and C ABI runtime;
  - `simjig`: kernel, model, typed handles, harness;
  - `simjig-macros`: `#[simjig::test]`;
  - `cargo-simjig`: `doctor`.
- C ABI version 1, vtable size 192, with trace, coverage and RNG slots.
- IR schema 2: adds `unrepresentable` constants.
- Scheduler: `Timer`s due at the same time fire one at a time, in registration order,
  each followed by every task it makes ready (and every task those wake), as cocotb runs
  its event loop after each Timer callback. Before, all of them woke together and tasks
  they woke ran after the last Timer task.
- Scheduler: value changes fire one (signal, edge kind) group at a time, oldest-primed
  first, each followed by every task it makes ready, as cocotb runs one VPI value-change
  callback at a time with its event loop inside each (README "Callback order"; goldens
  `coincident_join`, `changed_join`, `written_join`, `edge_kinds`). A task woken by one
  change that awaits a signal whose group another task already waits on, and which has
  not fired yet, now resumes in the same delta (it used to wait for the next change), and
  tasks those wakes make ready run before the next group's waiters. `rising`, `falling`
  and `changed` on one signal are separate groups. Wake order at one delta changes from
  global registration order to priming order; trace hashes of runs where that differs
  change.
- `first!`, `join!` and `with_timeout` run each operand in an internal helper task, as
  cocotb 2.1's `First`, `Combine` and `with_timeout` do (`_wait`): the helpers start at the
  front of the ready queue in operand order, the first helper to *finish* decides a
  `first!` (not the leftmost operand that is ready: an operand woken by a task the same
  edge woke loses to one the edge woke itself), losers are cancelled as cocotb cancels
  them (dropped when deregistered, finishing at their queue place), and the caller
  resumes when the last helper finishes (goldens `combinator_hop`, `loser_scheduled`,
  `spawned_combinators`, `cancelled_nest` and seven more in `test_callback_order`).
  Before, operands were polled inline in the caller, leftmost first, and the caller
  yielded once or twice afterwards. The cost is one helper entry and two or three extra
  polls of the caller's task per combinator. Trace hashes of runs with combinators
  change. `__private::{hop, boxed}` are gone (macro internals).
- Performance trade-off (correctness over speed, constitution priority order): each wait on
  a set `Event` or a free `Lock` now costs one extra scheduler round (one poll), as cocotb
  requires. On an Event/Lock-heavy benchmark (N tasks, each cycle: edge, set-event wait,
  lock, queue op) this is about 2.4x slower (N=1000: 14–18 ms → 33 ms; N=4000: 56 → 141 ms);
  the cost per poll fell from 70 to 55 ns. Other scheduler benchmarks are within ±10%.
- `Event::wait` on a set event and `Lock::lock` on a free lock resume after the tasks
  already ready (one hop), as cocotb 2.1's `Event.wait()` and `Lock.acquire()` do (goldens
  `event_ready`, `lock_ready`); they used to continue at once. The free lock is taken
  before the hop, so a task that runs in between sees it locked. Trace hashes of runs
  that await a set event or a free lock change.
- `Queue::get` and `Queue::put` that can complete do so at once even when other getters
  (putters) wait or were woken and have not run yet, as cocotb 2.1's `Queue` does (goldens
  `queue_get_ready`, `queue_put_ready`); the woken one finds the item (slot) gone and
  waits again, last in line. They used to queue behind every waiter. `Queue::try_put`
  likewise fails only when the queue is full (cocotb's `put_nowait`), no longer when
  puts are waiting.
- `yield_now()` ends only in a run of the task (or combinator helper) that awaits it. A
  hand-written join in a task's future that also awaits `first!`/`join!` used to see it
  end early, when the combinator's helpers ran.
- Performance: value-change groups are kept in a map keyed by priming order, so a walk
  over G groups costs O(G log G) instead of O(G^2); cancelling one of many waiters of an
  edge or a timer deadline, and firing one of many timers due together, is O(1) instead
  of O(waiters).
- `with_timeout` breaks ties as cocotb 2.1 does (golden `timeout_ties`): the timeout's
  Timer registers before the future's triggers and is checked first, so a `Timer`,
  edge or `cycles` due exactly at the timeout now times out (a future's own `Timer` used
  to win); a spawned task whose `Timer` was registered before the call still returns its
  result.
- `BlockedTask` implements `Display` (`` `main.d` awaits rising(clk) ``), the rendering
  `Failure`'s `Display` and the failure report share.
- `#[simjig::test]` has a default simulation-time budget of 10 ms (`DEFAULT_TIMEOUT_MS`),
  rounded up to a whole tick (`ceil(10 ms / tick)`, at least one) at precisions coarser
  than 1 ms; `timeout = "none"` disables it. Before, a test without `timeout` had no
  limit.
- `TestCtx::rng(path)` panics, at the second call, when `path` was already taken in the
  run (the message names the first call's location): two streams with one path would
  silently repeat each other's values.
- `FailureKind::TimeLimit` gains `blocked`: the tasks still waiting, and on what, as for
  `Starved` and `Deadlock`.
- New `FailureKind::ReadWriteLimit { limit, waiters }`: more than `READ_WRITE_MAX`
  (10 000) `ReadWrite` rounds that woke a task in one timestep.
- New `FailureKind::DpiPanic { message }`: a DPI-C import implemented in Rust panicked
  during an eval; the model is poisoned.
- Times print in the largest unit that shows them exactly (`1250 ns`, `1 us`; zero in
  the precision's unit, `0 ps`), through `SimTime::display` (`TimeDisplay`), in failures,
  reports and `summary.json`.
- The repro line is `SIMJIG_SEED=<seed> [<launcher> ]cargo nextest run -p <package>
  [--run-ignored all] -E 'binary(=<binary>) & test(=<test>)'`, with the launcher
  (`pixi run [--manifest-path ..] [-e <env>]`, `conda run -p <prefix>`) the test ran under.
- Run directories are `<root>/<package>/<binary>/<test>/<seed>/` (was
  `<root>/<test>/<seed>/`), written to a temporary sibling and renamed into place.
- IR schema 3: `Module::instances` (`Instance { name, module }`) and `Module::variables`
  (`Variable { name, kind: VariableKind::{Signal, Parameter, Localparam} }`), both omitted
  from the JSON when empty. Every IR hash, and so every generated `IR_HASH`/`kIrHash`,
  changes once.
- IR schema 4: `Module::generate_blocks` (`GenerateBlock { name, instances, variables,
  generate_blocks }`, named as Verilator names the scope: `g_x`, `genblk2`, `g[0]`) and
  `Instance::array` (an instance array), each omitted from the JSON when empty or false.
  Every IR hash, and so every generated `IR_HASH`/`kIrHash`, changes once.
- Probe paths may go through generate blocks (`top.g_x.u.x`, `top.genblk2.u.x`,
  `top.g[0].u.x`); naming a generate-for loop without an index, or an instance array, is
  a `codegen::Error::Probe` that names the fix.
- A probe whose module is a typo names the closest module ("did you mean `core`?"), and a
  probe path that ends at an instance or generate block says that it must end at a
  variable.
- `codegen::Error::Probe { path, reason }` (new variant) carries every probe mistake the
  lowering finds; `Dut::build` reports it as `Error::Config` ("probe `p` (declared in
  build.rs): ..."), the same headline as the probe checks against the scope table.
- Model and runtime archives are keyed in two steps, like the Verilator cache: every
  compile runs with `-MD`, and the key covers every file the compiler read (a header next
  to a recipe source, in an include directory or `-I` flag, or in the toolchain), plus
  the file names in each such directory. The build script also reruns on those files
  (except the ones under the cache and `OUT_DIR`). Every archive is compiled again once.
- Every archive key (models and Verilator runtimes) covers the compiler's identity: the
  executable its path resolves to, with size and modification time, and its `--version`
  output (run once per build script). Every archive is compiled again once.
- `Dut::verilator_args` also refuses `--sc`, `--main`, `--binary`, `--lib-create`, `-o`,
  `--hierarchical`, and the `--prefix=`, `--Mdir=`, `--top=` and `--top-module=` forms.
- The cache drops a model's object files once they are archived, rebuilds an entry
  directory that lost its archive, and removes work directories under `tmp/` that are
  more than an hour old.
- An interface port whose declaration cannot be read no longer hides the friendly
  unbound-port message for the other ports; it gets a line of its own.
- A `bind_iface` naming a port the DUT does not have ("no interface port `rq` on `ifdut`;
  did you mean `req`?"), an interface no source declares, or an interface other than the
  port's is an `Error::Config` naming the call to fix, instead of a dump of the generated
  top.
- `codegen::lower` checks each probe path against the IR's instance tree: a mistyped
  instance or variable (with the closest name), a declaring module that is not the
  instance's, and a `ProbeAccess::ReadWrite` probe of a parameter or localparam are
  `codegen::Error::Probe` errors, so the build fails before verilating.
- C ABI runtime: `create` refuses a probe bound for writing when its variable is a
  parameter (constant storage) or only `public_flat_rd` (`EXCEPTION`, model poisoned).
- After verilating, the build checks each probe the shim binds against Verilator's scope
  table (`V<prefix>__Syms__Slow.cpp`): a wrong width, an unpacked variable, a missing
  public scope or variable, or a writable probe of a constant fails the build
  (`Error::Config`) instead of the first `create`. `Emitted` has a new field `probes:
  Vec<ProbeSite>` (the probes the shim binds; `codegen_emitter` fills it, `Emitted::default()`
  leaves it empty), a breaking change for emitters that build `Emitted` without
  `..Emitted::default()`.
- C ABI: `cov_runs` is documented to cover code coverage counters only; covergroup bins
  (allocated when `new` runs) are in no run, while `cov_write` and `cov_zero` include them.
  The runtime stops noting coverage counters once create has built the runs.
- The build no longer emits `cargo::rerun-if-changed` for the generated inputs it stores in
  the cache (`inputs/<hash>/`: the `.vlt`, the interface wrapper, `verilator_input`s), so
  the first no-op build after a new DUT configuration no longer reruns the build script.
- A DUT whose interface ports are not all bound fails with one line per unbound port and
  the `.bind_iface(..)` call that binds it (`Error::Config`), instead of Verilator's error
  dump.
- `svtypes::error::Error::Tool::stderr` now holds the tool's whole stderr (it held the last
  40 lines); `Display` still shows the last 40.
- Compiled model archives are cached in the shared cache (`<cache>/archives/<key>/`,
  `libsimjig_model_<prefix>_<key16>.a`) instead of being compiled into `OUT_DIR` on every
  build-script run: editing one DUT recompiles only that DUT's C++. Every archive key
  (models and runtime) now also covers the files under `CppRecipe::include_dirs`.
- `svtypes` front-end errors with a source location print it first:
  `<file>:<line>:<col>: <message> (Verilator <version> JSON at <path>)`. A type error
  reached from a port names the port and its declaration, and an unpacked struct or union
  port says to declare the type `packed`.
- `Error::Command` leads with what the run was for (new field `what`, e.g. ``JSON pass for
  DUT `shreg` ``, ``verilating DUT `x` ``) and ends with the command line; its `status` is
  now the exit code (`Option<i32>`, `None` for a signal) instead of a string.
- `Dut::verilator_args` refuses the arguments the pipeline sets itself (`--prefix`,
  `-Mdir`/`--Mdir`, `--top-module`/`--top`, `--cc`, `--exe`, `--build`), each with the
  builder method or setting to use instead.
- `cargo simjig doctor` and the build's Verilator errors say "install Verilator 5.052 (in
  the simjig repo: `pixi install --locked`)" (and the same for the other tools) instead of
  assuming the simjig repository; the doctor's Verilator line says that builds keep the
  Verilator their build script found, so switching `PATH` alone does not rebuild.
- `svtypes --help` (`-h`, `help`) prints the usage and succeeds.
### Stage 3: DPI-C, waves, fork checkpoints, several DUTs per test

#### DPI-C imports implemented in Rust (`simjig`, `simjig-build`, `svtypes`)
- Generated code: for a design with `import "DPI-C"` functions or tasks, the generated
  module has a `Dpi` trait (`DutDpi` if the design names a type `Dpi`), one method per
  import, `Dut::set_dpi(impl Dpi)`, and one `extern "C"` trampoline per import (through
  `simjig::__private::dpi_import!`, so a crate with `forbid(unsafe_code)` compiles it).
  Snapshots: `codegen_it__dpi__*`, `codegen_it__snapshots__dpi__*`.
- New `simjig::Chandle` (an SV `chandle`).
- `dut_model!` takes an optional `dpi_imports = [..]`; `ModelMeta` gains `dpi_imports`
  (the C names of the Rust-implemented imports). Models without any skip the per-eval
  DPI scope.
- A panicking implementation fails the run as `FailureKind::DpiPanic`, with the panicking
  line in `Failure::location` and in the message; a call with no implementation, or a
  clash of import names, is `FailureKind::Setup` with what to change. Either poisons the
  model.
- **Limit:** two instances of one DPI build cannot run in one simulation (lockstep
  included): `set_dpi` cannot address the second one, whose first DPI call fails the run
  as `Setup`. Build the design a second time under another `simjig_build::Dut::new` name.
- The generated `Dpi` trait's docs say that an implementation can't drive another simjig
  model inside a call.
- Inside a DPI call every kernel entry point (signal reads and writes, triggers, `spawn`,
  `now()`, timers, sync primitives) panics with "DPI-C import `f` used <what>: DPI
  functions run inside eval; they can't access signals or await".
- `simjig_build::Dut::dpi_in_c(c_names)` (`DutConfig::dpi_in_c`) leaves those imports to
  C or C++ (for example in `CppRecipe::model_sources`): no trait method or trampoline is
  generated for them, so a C definition links without a duplicate symbol. An unknown name
  is an `Error::Config`. `DutConfig::rust_dpi_imports(&design)` lists the others.
- `simjig_build::codegen::{emit_dpi, dpi_prototypes, check_dpi_header,
  DpiHeaderMismatch}`; `emit_dpi` takes the import list. `RustOptions` gains
  `dpi_imports`. The build cross-checks the trampolines' prototypes against
  `V<prefix>__Dpi.h`.
- `svtypes::ir::{DpiImport, DpiArg, DpiType}` and `Design::{dpi_imports, dpi_exports}`.
  Both are left out of the JSON when empty, so designs without DPI serialise as before;
  they do not change the IR schema version.

#### FST waves (`simjig`, `cargo-simjig`)
- New `simjig::TraceOptions` (`new`, `depth`, `start`, `end`, `if_supported`,
  `with_path`, accessors) and `ModelOptions::{trace, with_trace}`. A traced model writes
  one sample per timestep it evaluated, at the end of the timestep.
- `LoadError::{NoTrace, TraceOpen}`.
- Waves on failure: `SIMJIG_WAVES=1`, `SIMJIG_WAVES=window:<t0>..<t1>` and
  `SIMJIG_WAVES_ON_FAIL=1` replay the test in a child process with tracing and write
  `waves.fst`, `waves.gtkw` and `replay/` to the run directory. `SIMJIG_TRACE`,
  `SIMJIG_TRACE_FROM` and `SIMJIG_TRACE_TO` trace a run directly.
- `FailureReport` gains `signals` and `waves` (`simjig::{WavesReplay, Verdict}`); the
  report's `waves:` line is now the working command `SIMJIG_WAVES=1 <repro>` (report
  snapshots changed in that line only). `TestOutcome` gains `precision`, `RunEnv` gains
  `waves` and `trace`. When no model of the test can trace, the line says so instead
  (`FailureReport::can_trace`).
- A replay keeps every trace file it wrote: one per model (`waves.fst`,
  `waves.model-<i>.fst`) and per fork branch (`waves[.model-<i>].branch-<j>.fst`), each
  with its own `.gtkw`. `WavesReplay::files: Vec<WavesFile>` (model, branch path, marker,
  save file; new `simjig::WavesFile`) replaces `WavesReplay::{fst, gtkw}`. When a fork
  branch fails the test, its files come first in the report, with the marker at the
  branch's own failure.
- `cargo simjig waves <test> --seed <seed> [--window ..]`. It checks `--window` itself
  (decimals such as `1.5us` become `1500ns`; a bad window is a usage error naming the
  forms), has `--help`, and after a failing test prints a note, not an error, keeping
  nextest's exit code.
- A traced model in a `fork` branch child continues its waves in
  `<stem>.branch-<i>.fst`; a child never writes the parent's file. The branch's file is
  finished (header written) also when the branch poisons the model: after a DPI failure
  it is closed normally, after a `$fatal` by destroying the model in the child.
- In a simulation of several models, model `i > 0` traces into `<stem>.model-<i>.fst`
  (new `ModelOptions::for_model`); model 0 keeps the given path.

#### Test context and time (`simjig`)
- New `TestCtx::precision()`, to print a time in a test body
  (`now().display(ctx.precision())`); `SimTime` documents why it has no `Display`.

#### Documentation
- `README.md`, `examples/quickstart` and the crate docs show the Stage 3 features. The
  quickstart's flop is built with `.trace_fst(true)` and gains a two-DUT test
  (`tests/inv_flop.rs`).

#### Failure kinds (`simjig`)
- New `FailureKind::Setup { message, help }`: the test's setup cannot run and the design
  is not at fault, with advice that says what to change. It replaces `ModelFatal` (whose
  help blamed the design) for a DPI-C call with no implementation or that cannot reach
  its instance, for every `LoadError` but `Create` (a stale build, `NoTrace`,
  `TraceOpen`, an unsupported precision), for DUTs that cannot share a simulation, and
  for a DUT type without `DutModel::{load, bind}` in a multi-DUT test.
- New `LoadError::help() -> Option<&'static str>`.

#### Several DUTs per test and lockstep (`simjig`, `simjig-macros`)
- `#[simjig::test]` accepts 2 to 4 DUT parameters, which share one simulation in
  parameter order. Five or more is a compile error.
- `DutModel` gains defaulted `load` and `bind`; new `simjig::LoadedModel`; `DutModel` is
  implemented for tuples of 2 to 4 DUTs. `dut_model!` emits `load` and `bind`.
- New `simjig::{Lockstep, Mirror, Divergence}`.

#### `fork()` checkpoints (new module `simjig::fork`)
- `run_from_checkpoint(seed, n, body)`, `Branch`, `Branches`, `BranchOutcome`,
  `BranchFailure`, `BranchError`, `ChildExit`, `BranchCost`, `RemoteFailure`,
  `BranchValue`, `ForkError`, `ForkHook` (with `at_child_exit`), `register_hook`,
  `HookGuard`. Unix only, with at most two threads in the process (nextest).
- `ForkError`'s `Debug` is its `Display`, so `.unwrap()` prints the advice.

#### Coverage counters
- New `Sim::coverage(model) -> Option<Vec<u32>>`: the counters of a model built with
  coverage, in the order of the ABI's `cov_runs`.
- New `Sim::zero_coverage(model)` (the ABI's `cov_zero`) and
  `Sim::write_coverage(model, path)` (`cov_write`: Verilator's `coverage.dat`, which names
  every point), both returning `Result<(), CoverageError>` (new `simjig::CoverageError`).
- New `TestCtx::{coverage, zero_coverage, write_coverage}`: the same, on the running
  simulation of a `#[simjig::test]`.

#### Build (`simjig-build`)
- An `-rpath` link argument is emitted once per build script, so FST models no longer
  make ld64 warn `duplicate -rpath`.
- Prefix claims in the cache moved to `prefixes/<scheme>/<prefix>`, the scheme naming
  the simjig-build version and the shape of `DutConfig`; a cache written by another
  simjig-build no longer reports every prefix as a collision.

#### Stage 3 review cycle 2 fixes
- Fork: a branch child runs `ForkHook::at_child_exit` only for hooks whose `in_child`
  succeeded. Before, a failing hook (a user's hook registered before a traced model, or
  a trace whose branch file could not be created) left the other hooks to finish, and a
  trace hook that never detached closed its inherited writer into the parent's FST.
- Traces: a fork child never closes or destroys a trace it did not reopen (one stopped
  before the fork, or whose reopen failed); C ABI `trace_reopen` now detaches the
  inherited writer (redirect to /dev/null, close) before checking the new path, so
  `IO_ERROR` leaves no trace open, and abandons the writer unclosed when the descriptor
  cannot be redirected.
- `ModelMeta` gains `module` (the generated module's `module_path!()`) and `dut_name()`;
  `dut_model!` fills it. A load failure's message starts with ``DUT `<name>`:``, and
  `LoadError`'s `Display` no longer repeats the advice its `help()` gives.
- `SIMJIG_WAVES_ON_FAIL=1` no longer replays a failure no model of which can trace;
  `SIMJIG_WAVES` still replays any run (the determinism check).
- The report of a test failed by a fork branch reads
  `t=<branch failure time> in fork branch <i> (checkpoint t=<time>)` and has no `help:`
  line of its own (each branch's help is in the message).
- `Lockstep::output` names both signals: `a.count vs b.count`, or
  `counter.count (model 0) vs counter.count (model 1)` when the names are equal.
- `simjig-build`: a failed Verilator run whose stderr has STMTDLY or NOTIMING ends with a
  hint to add `.timing(true)`.

#### Stage 3 review cycle 3 fixes
- Waves of a fork branch whose child died without a report (a signal): its file is
  labelled `died (killed by signal <n>); file incomplete`, listed after the readable
  files, has no marker (it used to be put at the checkpoint and labelled "failed at"),
  and gets no `.gtkw` or `gtkwave` command when the FST has no header. `WavesFile`:
  `marker` becomes `Option<SimTime>`; new field `died: Option<fork::ChildExit>`.
  `fork::ChildExit` implements `Display` (`killed by signal 6`, `exited with code 1`).
- New `ForkError::Nested`: `run_from_checkpoint` inside a fork branch is refused as
  "checkpoints cannot be nested inside a fork branch". It used to fail as `OtherTasks`,
  naming the branch's internal `main.fork_sentinel` task. `WavesFile::branch` becomes
  `Option<usize>` (it was a path of branch indices, one per checkpoint level), and the
  replay recognises only one `.branch-<j>` in a trace file name.
- DPI-C failures name the DUT (`ModelMeta::dut_name`, ``DUT `dpi` ``) instead of the
  model's prefix (kept where it is the point: two instances of one build, or another
  model's trampoline), and a `Setup` failure's message no longer repeats its help (DPI
  `Setup` failures, and a DUT type without `DutModel::load`).
- Docs: README "Coming from cocotb" (equivalents, deferred writes, pre-edge values,
  Timer ties, the time budget and limits, known differences); the Quick start installs
  the pinned tools (`cargo xtask setup`) and runs `cargo simjig doctor`; the quickstart
  README says how to use it outside this repository; `cargo simjig waves --help` says
  which test name to give.
