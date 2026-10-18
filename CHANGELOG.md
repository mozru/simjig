# Changelog

All notable changes to simjig. Constitution rule 4 requires an entry for every change to
the public API, the generated code or simulation semantics.

## Unreleased
- Each DUT's shim is written to `OUT_DIR/simjig/<name>/<prefix>_shim.cpp` (it was
  `OUT_DIR/simjig/<prefix>_shim.cpp`), and the shim of an earlier configuration is removed.
  A model archive's key no longer lists the files in `OUT_DIR/simjig/` or in the cache, so
  a rerun of the build script compiles only the models whose DUT changed (it compiled
  every model after any DUT changed, and most of them on the first rerun after a cold
  build). Every model archive is compiled again once.
- Initial workspace:
  - `svtypes`: the IR from Verilator `--json-only`;
  - `simjig-bits`: bit-vector values;
  - `simjig-build`: the pipeline, codegen and C ABI runtime;
  - `simjig`: kernel, model, typed handles, harness;
  - `simjig-macros`: `#[simjig::test]`;
  - `cargo-simjig`: `doctor`.
- C ABI version 1, vtable size 192, with trace, coverage and RNG slots.
- IR schema 2: adds `unrepresentable` constants.
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
  a `codegen::Error::Config` that names the fix.
- A probe whose module is a typo names the closest module ("did you mean `core`?"), and a
  probe path that ends at an instance or generate block says that it must end at a
  variable.
- `codegen::Error::Probe { path, reason }` (new variant) carries every probe mistake the
  lowering finds; `Dut::build` reports it as `Error::Config` ("probe `p` (declared in
  build.rs): ..."), the same headline as the probe checks against the scope table, where
  it used to be `Error::Codegen`.
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
  `codegen::Error::Config` errors, so the build fails before verilating.
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
