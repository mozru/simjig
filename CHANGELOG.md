# Changelog

All notable changes to simjig. Constitution rule 4 requires an entry for every change to
the public API, the generated code or simulation semantics.

## Unreleased
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
