# Proposal C: simjig architecture, optimised for the verification engineer's day

The yardstick is how long a verification engineer takes to get from "new interface" to
"running test", and from "red CI" to "I'm looking at the waveform at the failing
transaction". Decisions are judged on those two
times first, then on maintenance cost.

## 1. Guiding principles
1. **A failure is a product.** Every failure produces one record: kind, sim time,
   component path, task, location, signature, a one-line repro and a wave command. If a
   failure can't be triaged from its record alone, that's a simjig bug.
2. **Deterministic replay is the debug strategy.** Same seed and same binary give the same
   event log. We spend that guarantee on waves-on-failure (re-run instead of always
   tracing), on bucketing failures and on shrinking. Determinism is CI-tested.
3. **Our VIP gets no private powers.** `simjig-vip` is built only on the public `simjig` API, so
   a team's `acme-vip` crate is a peer, not a fork.
4. **Verilator is a dependency we version, not trust.** Pin it, detect it, run one front
   end per JSON dialect, use a versioned C ABI, and check everything at load (`doctor`).
5. **Small crates, one owner per concept, `unsafe` in one crate.** The scheduler is
   testable with a pure-Rust mock model and Miri, without Verilator.

## 2. Crate layout
```
svtypes ─┐                         simjig-bits      simjig-report   (leaves; no workspace deps)
         └─> simjig-build (build.rs)        │            │
                                         v            v
                                  simjig-sim  (executor, time, model FFI; ONLY unsafe crate)
                                         │
                                  simjig-verif (Bundle/Driver/Monitor, scoreboards, randomize,
                                         │   coverage, phases, end-of-test checks)
                                  simjig-vip   (ValidReady, AXI-Stream, AXI4-Lite)
simjig-macros-impl ─> simjig-macros ─┐         │
                               └──> simjig (facade: #[simjig::test] harness, run dirs, re-exports)
cargo-simjig (CLI) ─> simjig-report, svtypes, simjig-build          xtask ─> nothing depends on it
```
| Crate | Why it's a separate crate |
|---|---|
| `svtypes` | The IR is useful without the runtime: the cocotb back end and the `svtypes` CLI ship even if the M1.5 kill check fails. It holds the schema, the Verilator front ends and the Python emitter. |
| `simjig-bits` | Value semantics, proptested in isolation. Defines the `Packed` trait that generated types implement. |
| `simjig-report` | Owns the failure record, signature, run-dir schema and renderers. The CLI reads artifacts without linking the simulator, and its schema is a stable contract. |
| `simjig-build` | Build-time only: Verilator invocation, Rust/C++/`.vlt`/wrapper codegen, the cache. It never runs at test time. |
| `simjig-sim` | The kernel: executor, time wheel, triggers, delta loop, signal table, C ABI. The audited `unsafe` lives in `ffi/`, `exec/waker.rs` and `fork.rs`. |
| `simjig-verif` | Methodology, without `unsafe` or FFI. |
| `simjig-vip` | Bus models. It's the reference for "how to write VIP". |
| `simjig` | The one crate users depend on. It has one documented `prelude` for tests. |
| `cargo-simjig` | `doctor`, `triage`, `regress`, `waves`, `new-vip`. |

Invariants (go into ARCHITECTURE.md): leaves depend on nothing in the workspace;
`simjig-build` never depends on runtime crates; `simjig-vip` depends only on `simjig-verif` and
`simjig-sim`'s public API; only `simjig-sim` has `unsafe`.

## 3. Data flow
1. **User `build.rs`** (5 lines): `simjig_build::Dut::new("fifo").sources(glob).param("DEPTH",16)
   .probe("u_ctl.state", Access::Rd).build()?`.
2. **Toolchain probe:** `verilator --version` is checked against the supported table. A
   mismatch is an error that names the supported versions and how to switch
   (`VERILATOR_ROOT`).
3. **Elaboration pass:** `verilator --json-only --no-timing -G.. -D.. --top <top>`. The
   front end for that version builds the IR: it computes widths, parses literals and keys
   on `origName`. The IR is written to `OUT_DIR/fifo.ir.json`.
   **Interface-ported tops:** the testbench top owns the choice of interface
   specialisation, so the user states it once:
   `.bind_iface("s_axi", "axil_if", [("AW",32)], "slave")`. simjig-build generates a wrapper
   top, and the elaboration pass runs on the wrapper. If the user leaves it out, the
   Verilator diagnostic is mapped to an error that prints the exact `.bind_iface(..)` line
   to add.
4. **Codegen** from the IR alone: Rust types and the typed DUT, the per-prefix C++ shim,
   the `.vlt` (with `public_flat_rd/rw` only for declared probes) and the layout-check SV
   module.
5. **Model build:** `verilator --cc --prefix V<name>_<hash8> [--timing] [--coverage]
   [--trace-fst]`, plus `verilated.cpp` compiled with our `VL_USER_*` hooks, linked
   statically. Cache key: hash(sources, flags, Verilator version, simjig ABI). The cache is
   shared across crates in `target/simjig/cache`.
6. **Runtime:** `#[simjig::test]` creates `target/simjig/runs/<test>/<seed>/`. The scheduler
   streams `events.jsonl` (logs, transactions, phase marks) and writes `summary.json`.
7. **Reports:** nextest gives pass/fail and JUnit. `cargo simjig triage` buckets
   `summary.json` files by signature. Coverage goes to `coverage.dat`, merged with
   `verilator_coverage`. Waves are re-run on demand (§7).

## 4. Core runtime
**Time.** `SimTime(u64)` counts ticks of the DUT's time precision (taken from the IR).
User durations (`ns(5)`) are converted once, and a value that can't be represented is an
error, never rounded. Time never goes backwards; the debug and release builds both assert
this.

**Scheduler loop** (normative; the conformance suite enforces it):
```
loop per timestamp t = min(wheel.next(), model.next_time_slot() if --timing):
  ctx.time(t)
  ACTIVE: apply clock edges due at t (declaration order) + flush write buffer
          delta = 0
          loop { eval all models (declaration order)
                 compare watched signals with their last-eval values -> fire edge waiters
                 fire timers due at t; run ready tasks FIFO until the queue is empty
                 if write buffer empty: break
                 flush buffer; delta += 1; if delta > limit -> DeltaLimit{signals that toggled} }
  READONLY: fire ReadOnly waiters; any write -> ReadOnlyWrite failure (always on, cheap flag)
  poll DUT messages/finish/fatal; stop if test done, deadlock, or sim-time limit
```
**Writes are buffered deposits**, as in cocotb 2.x: `sig.set(v)` stages a write that
lands before the next eval. A read returns the value at the last eval. A second write to
the same signal by a *different* task in the same delta is a `MultipleDrivers` failure
naming both tasks and their spawn sites. A repeat write by the same task: the last one
wins. Buffering also makes synchronous cleanup possible, see cancellation below.

**Triggers and wakers.** `RisingEdge`, `FallingEdge`, `ValueChange`, `Timer`,
`ClockCycles`, `ReadOnly`, `NextTimeStep`, plus `Event`, `Queue`, `Lock`, `first!`/`join!`.
- Each registration is stored in a slab with a sequence number and a static description
  ("RisingEdge(dut.clk)"). Each trigger deregisters itself in `Drop`.
- Only signals that have waiters are compared after eval, so the cost scales with the
  number of waiters, not with the design.
- A waker is `(executor_id, task_index, generation)` packed into the RawWaker data word.
  It holds no `Rc` and does no allocation. A wake from another thread panics with a
  message. A stale generation is ignored.

**Tasks and cancellation.** Concurrency is structured. `tb.spawn_named("drv", fut) ->
Task<T>`: a task belongs to its parent's scope, and the scope's end cancels its children.
- `Task::cancel()` drops the future at the next scheduling point. Cleanup is synchronous
  `Drop`, which is enough because writes are buffered: a driver's `IdleGuard` deposits
  `valid=0` from `Drop`.
- Graceful drains use a cooperative `tb.shutdown()` future. This gives one cancellation
  mechanism, no `?` on every await, and no half-cancelled state.
- Trade-off: cleanup can't `.await`. That is deliberate (ergonomics > generality), and it
  is documented.
- Panics are caught per poll and become a `TaskPanic` failure with the task path. The
  remaining tasks are dropped.
- The harness keeps a task table. On deadlock (nothing runnable, no timers, clocks or DUT
  events) or on the wall-clock watchdog, it dumps every task, what it awaits, and since
  when.

**Signal handles.** Generated handles are `Copy` IDs: `In<T>`, `Out<T>`, `Probe<T>` and
`ProbeRw<T>` carry `SignalId` and `SimGen`. Direction is checked at compile time: `Out` has
no `set`. Access goes through the thread-local current `Sim`.
- A handle from another `Sim` or another model generation panics, naming the signal.
- The raw pointer lives only in `simjig-sim`'s signal table. It is read or written with
  `ptr::read`/`ptr::write` at the moment of use, with upper bits masked, and no reference
  survives an eval. Eval runs only while no task is being polled, so access can never
  overlap it.
- Why `Copy` handles rather than `Signal<'m>`: tasks are `'static`. A borrowing handle
  forces `Rc` plumbing into every VIP, while generation checks give the same safety at the
  cost of one compare.

**Multi-clock.** Clocks are scheduler-native objects (`tb.clock(dut.clk, ns(10))
.phase(ns(2)).duty(50)`), not tasks: cheap, ordered, visible in the event log. Edges at the
same `t` are applied together. Triggers on `public_flat_rd` derived clocks are allowed but
flagged `approximate` in the event log, and the docs show the intra-eval pulse limit.

**`--timing` DUTs.** These come from the IR flag and the build option. The shim exposes
`events_pending`/`next_time_slot`, and the loop takes the minimum with the wheel. With no
pending events and no testbench activity, the run ends as `Deadlock` or `Finished`.

**Multiple models** (lockstep): one `Sim` hosts N models, each with its own
`VerilatedContext` and prefix, evaluated in declaration order within the same delta loop.
simjig-build rejects DPI exports in lockstep builds.

## 5. FFI/shim boundary
| Item | Source |
|---|---|
| `simjig_abi.h` + `simjig-sim::ffi::abi` (`#[repr(C)]` mirrors, size asserts) | Hand-written, `SIMJIG_ABI_VERSION` |
| `simjig_runtime.cpp`: `VL_USER_FATAL/STOP/STOP_MAYBE/FINISH/WARN`, `VL_PRINTF` capture, context registry, exception barrier, FST open/dump/close, coverage write/zero | Hand-written, compiled once per model build |
| `V<prefix>_shim.cpp`: create/destroy/eval, signal descriptor table, `__Vcoverage` pointer, `static_assert`s on member sizes | Generated from the IR |
| Rust typed DUT + `simjig::__private::link_model!(prefix)` (the only `extern` in user code) | Generated from the IR |

The C ABI is **generic**: one function per model returns a vtable, and no per-signal FFI
calls exist:
```c
typedef struct { uint32_t id, width, words; uint8_t dir, access; void* ptr; } simjig_sig;
typedef struct {
  uint32_t abi_version; const char* prefix; const char* verilator_version; uint64_t ir_hash;
  void*      (*create)(const simjig_create_opts*, simjig_status*);
  simjig_status (*eval)(void*);                 /* never throws; returns OK/FINISH/STOP/FATAL */
  void       (*set_time)(void*, uint64_t);
  int        (*events_pending)(void*); uint64_t (*next_time_slot)(void*);
  const simjig_sig* (*signals)(void*, uint32_t* n);
  size_t     (*drain_msgs)(void*, simjig_msg*, size_t cap);  /* $display/$error/$fatal records */
  simjig_status (*trace)(void*, simjig_trace_cmd, const char* path, uint64_t t);
  uint32_t*  (*cov_counters)(void*, size_t* n);
  void       (*destroy)(void*);
} simjig_model_vtable;
```
At load, `abi_version`, `ir_hash` and the Verilator version are checked against what
`simjig-sim` and the generated Rust expect. A mismatch gives "model built by simjig-build ABI 3,
runtime ABI 4: run `cargo clean -p fifo_tb`".

**Fatal routing.**
- `$error` (`VL_USER_STOP_MAYBE`) records a message and counts toward `max_errors`.
- `$finish` sets the `Finished` status.
- `$stop` records a failure and finishes.
- `$fatal` and internal fatals (`VL_USER_FATAL`) record `{file,line,hier,msg,time}` and
  throw `simjig::FatalUnwind`, which the `eval` wrapper catches. The model is then
  **poisoned**: no eval, no `final()`. It's leaked; nextest's process-per-test makes that
  free. Its FST is closed so the waves up to the fatal survive.
- DPI imports (M2) are generated `extern "C"` trampolines with `catch_unwind` that
  dispatch to a per-`Sim` registered Rust impl.

## 6. Codegen and the IR
The IR schema (serde, JSON, `schema: 1`) is versioned separately from Verilator:
```rust
struct Design { schema: u32, producer: Producer /* verilator version, front end */,
  sources: Vec<SourceFile /* path, sha256 */>, defines: BTreeMap<String,String>,
  time_precision: TimeUnit, uses_timing: bool, top: ModuleId,
  types: Vec<Type>, modules: BTreeMap<ModuleId, Module> }
enum Type { Bits { width: u32, signed: bool, four_state: bool },
  Enum { name: QualName, base: TypeId, variants: Vec<(String, BigUint)> },
  Struct { name: QualName, packed: bool, members: Vec<Member /* name, ty, lsb */> },
  Union { .. }, PackedArray { elem: TypeId, dims: Vec<Range> },
  UnpackedArray { elem: TypeId, dims: Vec<Range> } }
struct Port { name: String, dir: Dir, ty: TypeId, member: String, storage: Storage /* C,S,I,Q,W(n) */ }
struct Module { orig_name: String, params: Vec<Param>, ports: Vec<Port>,
  probes: Vec<Probe>, iface_bindings: Vec<IfaceBinding>, opaque: bool /* --hierarchical */ }
```
- **Front ends per dialect:** `svtypes::frontend::verilator::{v5_04x, v5_05x}`, selected
  by version.
  - Strict mode is the default: an unknown node or field fails, naming its JSON path.
    `SIMJIG_UNTESTED_VERILATOR=1` downgrades that to a warning, and `doctor` reports it.
  - The golden corpus (SV files, then committed JSON per version, then insta IR snapshots)
    is the contract.
  - A nightly CI job runs the corpus against Verilator master, so format drift is seen
    before users see it.
- **Layout check:** the generated SV module asserts `$bits` of every type, and each struct
  member's offset, at elaboration. The shim adds a `static_assert` on each port member's
  storage size. A width-calculator bug therefore fails the build with a named
  type/member, never a test.
- **Back ends:**
  - Rust (`simjig-build::emit::{types, dut, shim, vlt, wrapper}`), built with `quote` and
    `prettyplease` and snapshot-tested.
  - Python/cocotb (`svtypes::emit::python`): dataclasses and IntEnums with pack/unpack,
    plus typed wrappers over `dut.<port>.value`. It reads the **IR**, never Verilator
    JSON, so cocotb users are insulated from dialect churn.

## 7. Errors, debuggability, observability
**One failure record** (`simjig_report::Failure`):
- **Fields:** `kind`, `time`, `component` (`tb.env.axis_in.drv`), `task`, `location`
  (Rust `#[track_caller]` or SV file:line), `message`, `notes`, `signature`.
- **Kinds:** `Check`, `Mismatch{diff}`, `DutError`, `DutFatal`, `DutStop`, `SimTimeout`,
  `WallHang{tasks}`, `Deadlock{tasks}`, `DeltaLimit{signals}`, `MultipleDrivers`,
  `ReadOnlyWrite`, `TaskPanic`, `EndOfTest{leftovers}` and `Abi`.
- **Signature:** a hash of the kind, the component path with indices normalised, the
  message with numbers and hex masked, and the location. It's stable across seeds, so a
  regression with 400 failures shows as 3 buckets.

The terminal footer of every failure looks like this:
```
FAIL fifo::backpressure  seed=0x9e3779b9  t=1_250.000ns  tb.env.sb  [Mismatch sig=7f3a21]
  expected Pixel { r: 0x12, g: 0x34, kind: Kind::Rgb }
  actual   Pixel { r: 0x12, g: 0x35, kind: Kind::Rgb }      (field-level diff, enum names)
  repro:  SIMJIG_SEED=0x9e3779b9 cargo nextest run -p fifo_tb -E 'test(=fifo::backpressure)'
  waves:  cargo simjig waves fifo::backpressure --seed 0x9e3779b9   (window 1_150..1_300ns)
  logs:   target/simjig/runs/fifo__backpressure/9e3779b9/events.jsonl
```
- **Waves on failure:** `cargo simjig waves` re-runs the same binary with
  `SIMJIG_WAVES=window:<t0>..<t1>`.
  - Tracing turns on at `t0` (or from the nearest fork checkpoint after M1.5). An FST is
    written, with a `.gtkw`/Surfer state file that holds a marker at the failure time and
    the failing component's bundle signals.
  - The re-run checks that it reproduces the same signature at the same time; otherwise
    it reports **non-determinism**, which is itself a bug.
  - Locally, `SIMJIG_WAVES_ON_FAIL=1` does this automatically.
- **Logs:** a `tracing` subscriber prefixes `[sim time] component level`.
  - Each task carries its component span, so paths are automatic.
  - Filtering: `SIMJIG_LOG=tb.env.axis_in=debug`. DUT `$display` output goes into the same
    stream with its `hier`.
  - Monitors record transactions (`start`, `end`, `stream`, `fields`) into `events.jsonl`,
    which Surfer or a script can overlay.
- **Supportability:**
  - Verilator diagnostics are re-emitted with the source line, and a table of known
    diagnostics carries a fix hint each.
  - `cargo simjig doctor` checks:
    - the Verilator path and version, and whether `VERILATOR_ROOT` agrees with what build.rs used;
    - the C++ compiler and ccache;
    - FST support;
    - nextest;
    - whether the cache is stale;
    - whether fork is available.

    Each finding comes with a fix command.
- **Regressions:** nextest runs the tests (partitioning, JUnit). `cargo simjig regress
  --seeds 200` drives nextest in seed batches and collapses the `summary.json` files into
  a bucket table. It lists the first seed per bucket and its repro line, and diffs the
  buckets against a stored baseline ("new", "fixed", "still failing"). Retries are off,
  because a flaky test is a determinism bug.

## 8. Testing the framework
| Layer | How |
|---|---|
| `simjig-bits` | proptest against a `BigUint` reference. IEEE tables for `Logic` later. |
| `svtypes` | Golden corpus per Verilator version, insta IR snapshots, proptest of the width calculator against the literal parser, old-IR-deserialises tests. |
| `simjig-build` | Snapshots of the Rust, C++, `.vlt` and wrapper output. trybuild: `Out::set` doesn't compile and a misnamed bundle field gives a spanned error. |
| `simjig-sim` | A `MockModel` (pure-Rust comb adder, flop, counter, timing model) drives the scheduler under **Miri**. Event-log snapshots per scenario. |
| Conformance | About 30 toy DUTs, the same scenario in cocotb and simjig, with normalised `(time, phase, trigger, value)` logs diffed nightly. |
| Error UX | One fixture per `FailureKind` that triggers it and snapshots the rendered footer. A wording change goes through review. |
| VIP | Loopback through passthrough RTL, AXI-Lite against a register-file RTL, randomized backpressure. |
| Determinism | Every example runs twice and the `events.jsonl` hashes must be equal. |
| FFI | ASan/UBSan job (Rust and C++), and the fatal-unwind path over seeded `$fatal` and array-bounds fatals. |

## 9. Extension points
- **New custom interface in minutes:** `cargo simjig new-vip myif --from rtl/myif.sv`
  scaffolds a `#[derive(Bundle)]` struct from the IR, a driver, a monitor and a loopback
  test. Binding by prefix lists missing or extra signals with the closest names.
- **4-state:** the IR already has `four_state`. Codegen adds `T4` views, the signal
  descriptor gains `planes: 2`, and `get_planes`/`set_planes` go into the shim. Edge
  detection is generic over `BitVector`. The scheduler doesn't change.
- **AXI4 full:** a new `simjig-vip::axi4` module. The ID router and out-of-order scoreboard
  already exist in `simjig-verif`.
- **SystemRDL:** a new `simjig-rdl` crate (PeakRDL Rust exporter). Frontdoor access goes
  through `trait RegBus` (AXI-Lite master implements it). Backdoor access uses `ProbeRw`
  handles that the RDL generator requests through the same `.probe()` API.
- **Properties project:** stable hooks are a synchronous per-timestep `Observer` (sampled
  in ReadOnly), `simjig_report::fail()`, and coverage registration. It needs no executor
  access.
- **Hierarchical SoCs:** `Module.opaque` exists. Probes into opaque blocks are rejected at
  build time with the block named, and models are built with `--hierarchical`.

## 10. Key type sketches
```rust
// simjig-sim
pub struct SimTime(u64);
pub struct Signal<T: Packed, D: Direction> { id: SignalId, generation: SimGen, _p: PhantomData<(T, D)> } // Copy
impl<T: Packed, D: Readable> Signal<T, D> { pub fn get(self) -> T; pub fn get_bits(self) -> Bits<{T::WIDTH}> }
impl<T: Packed, D: Writable> Signal<T, D> {
    #[track_caller] pub fn set(self, v: T); /* buffered deposit, masked */ }
pub fn rising_edge<D: Readable>(s: Signal<Bit, D>) -> Trigger;   // impl Future<Output = ()>
pub struct Task<T> { id: TaskId, _p: PhantomData<T> }
impl<T> Task<T> { pub fn cancel(self); pub async fn join(self) -> Result<T, Failure> }

// simjig-verif
#[expect(async_fn_in_trait, reason = "single-threaded runtime, static dispatch")]
pub trait Driver { type Item; async fn drive(&mut self, item: Self::Item); }
pub trait Monitor { type Item; fn stream(&self) -> Queue<Stamped<Self::Item>>; }
pub trait Predictor { type In; type Out; fn predict(&mut self, i: &Self::In) -> Vec<Self::Out>; }
pub trait EndOfTest { fn check(&self) -> Result<(), Vec<Leftover>>; } // scoreboards, ID trackers

// user test
#[simjig::test(dut = Fifo, sim_timeout = "2ms")]
async fn backpressure(tb: &mut Tb<Fifo>) -> simjig::Result {
    let dut = tb.dut();
    tb.clock(dut.clk, ns(10));
    tb.reset(dut.rst_n, ResetLevel::Low, cycles(5)).await;
    let (mut drv, mon) = (AxisSource::new(tb, "in", dut.s_axis), AxisSink::new(tb, "out", dut.m_axis)
        .ready_policy(Backpressure::random(0.3)));     // separate seeded stream
    let sb = tb.scoreboard(InOrder::new("sb"), mon.stream());
    for pkt in Pixel::strategy().vec(1..64).sample(tb.rng("stim")) {
        sb.expect(pkt.clone()); drv.drive(pkt).await;
    }
    tb.quiesce(cycles(100)).await               // then mandatory EndOfTest checks
}
```

## 11. Risks, and what we won't build
| Risk | Mitigation |
|---|---|
| Verilator JSON drift | A front end per dialect, strict mode, the nightly master canary, and the IR decoupling cocotb users. |
| Throwing through Verilator code on a fatal | The model is poisoned and leaked, never reused. The ASan job runs over the fatal fixtures. |
| Divergence from cocotb semantics | The conformance suite, with the normative loop above. |
| C++ build time kills the edit loop | The content-hash cache shared across crates, a ccache check in `doctor`, and the IR pass skipped when the source hash is unchanged. Target: a test-only edit takes under 5 s to re-run. |
| `Copy` handles through thread-local state hide mistakes | Generation checks, plus panics that name the signal and both sims. |
| Waves-on-replay depends on determinism | Determinism is CI-tested, and a re-run detects its own divergence. |

**Deliberately not built:**
- a waveform viewer or an HTML dashboard (we emit FST, `.gtkw`, JSON and JUnit);
- a test runner (nextest is the runner);
- UVM factory, config_db or phase jumping;
- a `simjig.toml` signal-mapping language;
- soft cancel with `Result` at every await;
- a multi-threaded executor;
- VPI;
- Python bindings for the runtime;
- a constraint solver;
- `--savable` checkpoints (beyond an optional disk snapshot later);
- retries for flaky tests.

## 12. Milestones
| Milestone | Contents | Exit |
|---|---|---|
| **M0** | `svtypes` v5 dialect front end and corpus; IR schema 1; layout check; generic C ABI and runtime shim with fatal routing; typed DUT; `simjig-sim` with one clock, Timer/edge triggers and the delta loop on `MockModel`; `simjig-report` failure record and footer; `doctor` v0. | FIFO test passes with zero hand-written types. `$fatal` produces a record, not `exit(1)`. |
| **M1** | Full trigger set, structured tasks, cancellation, deadlock/hang dumps; the conformance suite; `simjig-verif` (Bundle, Driver/Monitor, InOrder/OOO scoreboards, Randomize, sequences, quiesce, EndOfTest); ValidReady and AXI-Stream; run dirs, signatures, `cargo simjig triage/waves`; Python back end; `new-vip`. | Struct pipeline, width converter and AXI-S FIFO verified. Every FailureKind snapshot is green. |
| **M1.5** | `fork.rs` checkpoints (`--threads 1`); proptest shrinking that re-runs from the post-reset fork; lockstep of two prefixes; libFuzzer over `cov_counters`; waves from the nearest checkpoint. Pre-registered kill check against cocotb. | The seeded FIFO bug shrinks to a minimal case. The kill-check numbers are published. |
| **M2** | Multi-clock wheel and async FIFO CDC; `--timing` DUTs; covergroups with merge and a holes report; AXI4-Lite master/slave; `simjig-rdl` RAL; DPI trampolines; `cargo simjig regress` with baselines. | AXI-Lite register-block test with RAL and a coverage closure report. A 200-seed regression bucketed in one table. |
