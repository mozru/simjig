# simjig architecture: decision record

simjig is the Verilator-Rust interface layer: types, build, model access, scheduling,
time, errors and waves. Verification methodology (bus models, scoreboards, constrained
random, shrinking, fuzzing harnesses, covergroups, RAL) is out of scope and belongs to
projects built on top. simjig exposes what such layers need: coverage counters, several
models per simulation, checkpoints and hooks.

The design was chosen from three independent proposals: a systems design in the TigerStyle
manner, a Rust-library design, and a verification-tooling design. This file records what
was decided and why, and the alternatives that lost. `ARCHITECTURE.md` has the code map.

## 0. Scheduler semantics: the conformance goldens are normative
`tests/conformance/README.md` ("Observed semantics") and `golden/*.jsonl` (cocotb 2.1.0 on
Verilator 5.052) **override** the scheduler description below wherever they differ:
- Timers due at t resume before that time step's clock edges.
- Edges on testbench-driven clocks see the **pre-edge** state. New flop values are visible
  from ReadOnly.
- Edges on design-driven signals see post-eval values.
- Writes are deposits applied after the next eval. A write right after `RisingEdge` is not
  captured by that edge.
- Within a delta the last write wins. `WriteConflict` applies only to two different tasks
  writing different values.
- Within one value-change comparison, waiters wake one trigger group (signal, edge kind) at
  a time, in priming order, running all ready tasks after each group (see the conformance
  README, "Callback order").

## 1. Decisions all three proposals shared
- **Signal handles are `Copy` ids** (model, generation, slot), with the direction in the
  type. They are resolved through the running kernel. No Rust reference to model memory
  ever exists, and a stale handle panics, naming the signal.
- **One cancellation mechanism: drop.** Cleanup is synchronous `Drop`; a graceful stop is a
  cooperative stop token. There is no soft-cancel protocol. Dropping a `Task` handle
  before its task ever ran is still a cancellation, but fails the run as `Setup`
  (`let _ = spawn(..)` would otherwise pass silently); the rule and its exemptions are in
  `simjig::kernel::tasks` (`settle_unrun_drops`).
- **Structured tasks.** Each poll runs under `catch_unwind`, and a panic becomes a
  `TaskPanic` failure carrying the hierarchical task name.
- **One scheduler loop per distinct timestamp:** apply the clock edges due at t; eval; wake
  waiters on changed watched signals; run tasks; repeat until quiet, up to a delta limit;
  then ReadOnly, where any write is a failure; then advance to the earlier of the timer
  wheel and `nextTimeSlot()`. Clocks are kernel data, not tasks, and coincident edges
  share one eval. Triggers on internal derived clocks are sampled: they see the clock
  after the eval that changed it.
- **One front end: Verilator `--json-only`**, with `--timing` iff the DUT is built with it,
  decoded in one place per Verilator dialect, with widths computed (never trusted), strict
  about unknown nodes, and backed by a fixture corpus and IR snapshots. slang is not used;
  it could return as a second IR producer.
- **Layout check:** a generated SV module asserts `$bits` and member offsets, and the shim
  `static_assert`s member sizes.
- **C ABI:** one generated `simjig_vtable_<prefix>()` per model, carrying `abi_version`,
  `ir_hash` and the Verilator version, all checked when the model loads. A small
  hand-written C++ runtime provides the `VL_USER_*` hooks and the exception barrier.
- **Fatal routing:** record `{file, line, hier, msg, time}` and throw `FatalUnwind`. The
  eval wrapper catches it, and the model is poisoned (no further eval). Plain models
  unwind cleanly (`docs/notes/abi.md`). `--timing` models record the fatal and return
  FATAL after eval, because throwing out of a coroutine aborts. An internal Verilator
  fatal inside a coroutine still aborts the process, and nextest's process-per-test
  contains it.
- **Failures are data** (`Failure { kind, time, precision, task, location, message }`;
  the harness's report adds the seed, the signature and the repro), with an actionable
  `Display`, a one-line repro, and waves from a deterministic replay. The replay doubles
  as the determinism check. Retries are never used.
- **The framework's own tests** use a pure-Rust fake model behind the same backend trait,
  under Miri, plus random kernel programs, the cocotb conformance suite, C++ tests of the
  runtime, and snapshots of every failure rendering.
- **`fork()` checkpoints** (Unix, `--threads 1`).
- **Not built:** `--savable`, VPI, a solver, a `.toml` mapping language, a UVM
  factory/config_db, a multi-threaded executor, a wave viewer, retries.

## 2. Decisions where the proposals disagreed
| # | Topic | Decision | Alternatives | Why |
|---|---|---|---|---|
| 1 | Crate count | **Few, deep crates**, split only for a named need (`simjig-target-dir`, `cargo-simjig`) | 12 to 14 crates | Constitution rule 6: flat and deep. |
| 2 | Testbench writes | **A write log applied before each eval** | immediate `ptr::write` into the model | Matches cocotb 2.x deposit semantics and gives deterministic conflict detection (`WriteConflict` names both tasks). `Drop` cleanup still works, and every stimulus bit is traceable. |
| 3 | Randomness | **Named streams derived from `hash(root_seed, name)`** (`harness::seed`) | one recorded choice stream for randomization, shrinking and fuzzing; proptest strategies | Streams stay independent when a test adds one. Shrinking and fuzzing are methodology, out of scope. proptest stays a dev-dependency for `simjig-bits`. |
| 4 | Logging | **A fixed-size flight recorder**, dumped to `events.jsonl`; its trace hash is the determinism check. `tb_log!` prints a timed line to stdout | `tracing` with `events.jsonl` | Deterministic and allocation-free. One event model serves triage and replay. |
| 5 | Codegen writer | **A plain indenting writer for every emitter**, fed by **one lowering** | `quote` + `prettyplease` | C++, `.vlt` and SV need a text writer anyway. One mechanism gives byte-stable output and fast build scripts. |
| 6 | Cross-checks | **The SV layout module and a header check** (`VL_IN/OUT(msb,lsb)`) on every build | either one alone | The header check is cheap and catches drift on every build, not just in CI. |
| 7 | Enums | **A newtype with consts plus a fallible `variant()`** | a Rust enum whose read fails on an invalid value | SV enums can legally hold non-member values. Reads stay honest, and the test decides what an invalid value means. |
| 8 | Time unit | **Ticks of the design's `timeprecision`** | picoseconds, rejecting finer precision | Supports fs-precision designs. Durations that cannot be represented are errors, never rounded. |
| 9 | Interface-ported tops | **`.bind_iface(..)`**, and if it is missing, an error that prints the line to add | a wrapper with unspecified parameters | The testbench is the real instantiator. The error tells the user exactly what to write. |
| 10 | libverilated | **Built once per (Verilator version, hook defines) in the shared cache**, by `simjig-build` | per-model `verilator --build`; a `-sys` crate | Gets the build-time win without a sys crate. |
| 11 | Tooling CLI | **`cargo simjig doctor`, `waves` and `show`** | `triage`, `regress`, `new-vip` | Supportability is a goal; `doctor` makes setup reproducible. Methodology commands are out of scope. |
| 12 | `unsafe` location | **A few named modules of `simjig`** (`ARCHITECTURE.md`, "Invariants"); the workspace denies `unsafe_code` elsewhere | a `-sys` crate; one unsafe crate | Same safety without an extra crate. |

## 3. Crates
`ARCHITECTURE.md` has the crates and what each owns. Split points kept in reserve: `simjig::kernel` becomes its own crate when a properties
project needs it; the report becomes its own crate when `cargo-simjig` must read run
directories without linking the simulator.

## 4. Questions settled by experiment
Each was open when the design was chosen; `docs/notes/spikes.md` and `docs/notes/abi.md`
have the experiments.
1. A C++ exception through Verilator-generated code unwinds cleanly for plain models;
   `--timing` coroutines record the fatal instead (above).
2. Looking up a signal through the thread-local kernel on every access had a budget of
   5 ns or less, with base pointers cached in `Dut` as the fallback (the API stays the
   same). The measured costs, and the storage pointers that remove most lookups, are in
   `docs/notes/perf.md`.
3. `fork()` works inside a nextest test process, with `contextp->threads(1)`, no other
   active threads, and no inherited trace file.
4. Verilator's JSON for interfaces, modports and parameter clones on 5.052 is described
   in `docs/notes/svtypes.md`.

## 5. Notes
- Marlin (Rust, Verilator as a library) is MPL-2.0, not MIT (checked against its LICENSE
  file). Borrow ideas, not code.
- Driving on the falling edge with two evals per cycle and no `--timing`, as an earlier
  Rust style guide advised, is superseded by the per-timestamp loop and `--timing`
  support above.

## 6. API conventions
- **Rust-native signal access:** `sig.set(v)` and `sig.get()`. Plain assignment can't be
  overloaded in Rust, and operator tricks (`<<=`, implicit reads in `==`, macro blocks)
  are rejected because they hide reads and writes of the model.
- `set` takes `impl Into<T>`, so literals work (`dut.data.set(0x5a)`). A value that
  doesn't fit fails at the caller's line (`#[track_caller]`), naming the signal and its
  width.
- 1-bit signals use `Bit`, which converts from `bool` and from `0`/`1`.
- Triggers are functions returning named future types (`timer(d)` returns `Timer`), as
  Tokio's `sleep` returns `Sleep`; cocotb's trigger classes become snake_case functions.

## 7. Verilator facts the design rests on
- **`--json-only`** is produced after parameter specialisation. It has no `width` field
  (widths must be computed), member `lsb` is a string omitted when 0, values are Verilog
  literals, the format changes between releases, and it turns `--timing` on unless
  `--no-timing` is passed. A top with interface ports cannot be elaborated.
- **Model access:** ports are members of the model object. Internal signals are reachable
  only if marked `public_flat_rd`/`public_flat_rw`; writing other internals is undefined
  behaviour, and public signals cost performance. Upper bits must be masked on writes.
- **Fatals:** `$fatal`, `$stop` and internal fatals call `std::exit(1)` unless the
  `VL_USER_FATAL`/`VL_USER_STOP_MAYBE` hooks or `fatalOnError(false)` are used.
- **Eval and time:** edges are detected against the value at the last eval. `--timing`
  models expose `eventsPending()` and `nextTimeSlot()`. A pulse on an internal derived or
  gated clock inside one `eval()` is invisible to the testbench.
- **`--savable`** is incompatible with `--coverage` and `--timing` and cannot save
  testbench futures, so checkpoints use `fork()` with `--threads 1`.
- **Several models in one process** each need their own `--prefix`; DPI symbols are
  global, and the `$urandom` state is per thread context.
