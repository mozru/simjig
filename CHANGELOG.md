# Changelog

All notable changes to simjig. Constitution rule 4 requires an entry for every change to
the public API, the generated code or simulation semantics.

## Unreleased
- README: "A parameter sweep", writing each test once against a user trait
  implemented for each configuration's `Dut`.
- `simjig::now_display()` prints the current time in physical units without the
  precision (`now().display(ctx.precision())` needed `ctx`), and `simjig::log!(..)`
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
