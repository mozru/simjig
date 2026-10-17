# Proposal A: simjig architecture (TigerStyle / SQLite lineage)

## 1. Guiding principles
I optimise for **one maintainer holding the whole system in their head for ten years**.
Throughput and features come second.

1. **Determinism is a feature you can test.** Same seed and same binary give the same
   bit-exact event trace. Every failure can be replayed for free, so "waves on failure"
   just means re-running the test with tracing on.
2. **Plain data at every boundary.** The IR is a struct of `Vec`s indexed by typed ids.
   Triggers are an enum. Signal handles are `Copy` ids. Failures are an enum. Behaviour
   lives in a few deep modules that act on that data.
3. **Explicit limits, asserted always.** Task slots, delta cycles, queue capacity,
   rejection attempts and sim time all have named constants, and exceeding one is an
   actionable failure, not a hang. Assertions stay on in release builds, because a
   testbench is not a hot server.
4. **One mechanism per concept.** One front end (Verilator JSON), one randomness source
   (the choice stream, which also feeds shrinking and fuzzing), one cancellation (drop),
   one C ABI (a fixed ops table), one log (the flight recorder).
5. **Minimal dependencies.** The runtime depends on nothing outside the workspace. Build
   time uses `serde_json` and nothing else. Only the proc-macro crate uses `syn`/`quote`.
   No tokio, proptest, tracing or cc.

## 2. Crate layout

```
                 simjig-macros (syn, quote)
                     ^
simjig-bits  <----  simjig  <----  simjig-vip            (test-time)
                     ^
svtypes   <----  simjig-build                       (build-time, from build.rs)
(serde_json)    (no deps)
```

| Crate | Owns | Why it is a boundary |
|---|---|---|
| `svtypes` | Type IR, Verilator `--json-only` front end, width calculator, Verilog literal parser, Python/cocotb emitter, `svtypes` CLI | Ships standalone whatever the M1.5 kill check decides. Pure, `forbid(unsafe)`. |
| `simjig-bits` | `Bits<N>`, `SBits<N>`, `cat!`, `sv!`, explicit width changes | Value semantics with no I/O, ideal for proptest/Miri. No deps. |
| `simjig-build` | Rust/C++/`.vlt`/wrapper-SV emitters, running `verilator --build`, the content-hash cache, the hand-written shim sources (`shim/simjig_shim.h`, `simjig_shim.cpp`) | Runs only inside `build.rs`, so it must never link into tests. |
| `simjig` | Kernel (executor, time wheel, phases), model FFI, signals, sync primitives, failures, recorder, choices, scoreboards, coverage, lockstep, fork checkpoints, fuzz glue | **One deep crate.** Splitting off `-sys` or `-runtime` adds pass-through layers. Only three modules contain `unsafe`: `model/raw.rs`, `kernel/waker.rs` and `fork.rs`. |
| `simjig-macros` | `#[simjig::test]`, `#[derive(Bundle)]`, `#[derive(Arbitrary)]` | Proc-macro crates must be separate. Kept thin: expansion logic is in modules unit-tested with `proc_macro2`. |
| `simjig-vip` | ValidReady, AXI-Stream, AXI4-Lite (later AXI4, APB) | Uses only `simjig`'s public API. That proves the API is enough and keeps bus churn out of the kernel. |

I am dropping **slang** as a second front end. Two front ends means two naming schemes
and twice the fixtures. The cocotb back end consumes the same IR.

## 3. Data flow
1. **`build.rs`** calls `simjig_build::Dut::new("fifo").sources(..).top(..).param("DEPTH", 16).public_rd("u_ctl.state_q").build()`.
2. **Front end.** `verilator --json-only --no-timing` (plus the user's `-G`/`-D` flags)
   runs on the wrapper top. `svtypes::frontend` decodes the result into `Ir` and then
   *validates* it: every width is recomputed and asserted, enum values are parsed, and
   unknown nodes or fields cause a hard error naming the node, file:line and the Verilator
   version.
3. **Emit**, into `OUT_DIR`, deterministically from sorted ids:
   - `fifo.rs`: the types, `Dut` handles, params as `const`s, and `Arbitrary` impls;
   - `fifo_model.cpp`: the signal table and the ops table instance;
   - `fifo.vlt`: `public_flat_rd/rw` for the requested internals only;
   - `fifo_wrap.sv`: only when the top has interface ports;
   - `fifo.ir.json`: kept for debugging and for the Python back end.
4. **Build.** `verilator --build --prefix Vfifo_<hash8> -j` builds the design with the
   shim `.cpp`s on the command line. That produces `libVfifo_<hash8>.a` plus
   `libverilated.a`, which are linked through `cargo::rustc-link-lib`. The cache key is
   FNV-1a-128 over (sources, flags, Verilator version, simjig-build version). The cache lives
   in `target/simjig-cache/<key>`, so test crates share it.
5. **Runtime.** `simjig::dut!(fifo)` does the `include!`. `#[simjig::test]` creates the
   `Sim`, constructs the model, runs the async body and returns a `Report`.
6. **Reports.** Each run writes to `target/simjig/<test>/<seed>/`:
   - `report.txt` (failures, stats, repro line);
   - `recorder.log` (the last N kernel events);
   - `choices.hex` (the minimal failing stimulus after shrinking);
   - `waves.fst` (on re-run);
   - `coverage.dat` (Verilator format, so `verilator_coverage` does the merging).

## 4. Core runtime

**Ownership.** `Sim` owns everything:
- the `Model`s (each one a C handle plus its signal table);
- the task slab (`MAX_TASKS = 4096`, generation-tagged `TaskId`);
- the time wheel (`BTreeMap<SimTime, Vec<WheelEntry>>`);
- the trigger tables (per-signal waiter lists, ordered by registration sequence);
- the ready queue (FIFO `VecDeque<TaskId>`);
- the recorder ring buffer.

While `Sim::run` is on the stack, a thread-local `Cell<*const Kernel>` points at it. A
guard clears it on exit, including during unwinding.

**Signal handles are `Copy` ids, not borrows.** A handle is
`Sig<T> { model: ModelId, epoch: u32, index: u32 }`. It has no lifetime and no `Rc`, so
moving one into a spawned `'static` task just works. Each access goes through the current
kernel:
- check the epoch (it catches a handle from a previous `Sim`);
- look up `SigDesc { ptr, width, storage }`;
- `ptr::read` or `ptr::write` with the storage class (`CData`/`SData`/`IData`/`QData`/`VlWide`);
- mask the upper bits on writes.

No Rust reference to model memory ever exists, so nothing can be held across `eval()`.
Access outside a running sim panics, naming the signal. The handle type comes from the IR:
`Sig<Bits<8>>`, `Sig<SeqCmd>`, `Sig<State>`. Reading an enum signal that holds a
non-member value fails the test ("`dut.state` = 5 is not a member of `state_t`"). The
`get_raw()` method returns `Bits`. Write rights are encoded in the type: `Out<T>` only has
`get`, `In<T>`/`InternalRw<T>` add `set`, `InternalRd<T>` is read-only.

**Phases (normative, one loop per distinct timestamp t):**
1. `set_time(t)`. Apply every kernel-owned clock edge due at t, in declaration order, then
   `eval`. Clocks are data (`Clock { sig, period_ps, phase_ps, high_ps }`), not tasks.
2. **Detect.** Compare watched signals against their snapshot from the last eval, and wake
   their waiters in (signal id, registration seq) order. Run the ready queue until it is
   empty. If any task wrote, `eval` and detect again. `DELTA_MAX = 64` iterations gives
   `Failure::DeltaLimit`, listing the signals that toggled in the last iteration.
   **Write conflicts:** two different tasks writing the same signal in one delta fail
   immediately, naming both tasks.
3. **ReadOnly.** Wake `ReadOnly` waiters. Any write is a failure. This check is always
   on: it costs one flag test.
4. **Advance.** t' = min(wheel head, `next_time_slot()` if `events_pending()`). If there
   is nothing at all and tasks are still blocked, the result is `Failure::Deadlock`, which
   lists each blocked task and what it awaits. Time is `SimTime(u64)` in picoseconds.
   A design whose time precision is finer than 1 ps is rejected at construction.

**Triggers** are one enum, `Wait::{Edge(sig, Edge), Time(SimTime), ReadOnly, NextStep,
Event(EventId), Queue(QueueId)}`. Awaiting one registers a `Waiter { task, seq }` in the
kernel table. The trigger future's `Drop` removes it, so cancellation never leaves a
dangling wake. Wakers exist only for foreign combinators. Their data pointer *is* the
packed `TaskId` (no allocation). `wake` asserts the current thread and pushes onto the
kernel ready queue.

**Tasks and cancellation.** There is only one kind of cancellation: **dropping**.
Cleanup is synchronous `Drop`, and because signal writes are synchronous, a driver's guard
can lower `valid` in `drop`. A two-kind scheme (soft cancel that delivers an error at every
await) makes every await fallible for little gain. The concurrency primitives map to SV:
- `spawn` is `fork … join_none`: the task is owned by the enclosing `scope` and dropped
  when the scope or the test ends;
- `join!` is `fork … join`;
- `first!` is `join_any` followed by `disable fork`;
- `handle.await` waits for one spawned task.

Each poll runs under `catch_unwind`. A panic becomes `Failure::TaskPanic { task_path,
location, time }`. Tasks have hierarchical names (`env.axis_in.driver`), which the
recorder and failures use.

**Sync:** `Event`, `Queue<T>` (always bounded), `Lock`, `with_timeout`. All of them are
plain data in kernel slabs and are woken via `Wait::Event` and `Wait::Queue`.

**Multi-clock.** Each clock adds its next edge to the wheel. Coincident edges are applied
together before a single eval, which is what makes CDC tests (an async FIFO) correct.

**`--timing` DUTs** are built with `--timing`. After each eval the kernel asks the ops
table for `events_pending`/`next_time_slot` and merges the answer into step 4. Internal
`#`-driven clocks are then observable exactly at slot boundaries. Edges on internal
derived or gated clocks are *sampled*: the recorder notes this once per signal, and the
docs state the limit.

**Multiple models** (lockstep) share one `Sim` and one thread. The shim calls
`Verilated::threadContextp(ctx)` before every eval, so each model keeps its own
`$urandom` state. DPI imports are global symbols, so models that export DPI cannot be in
lockstep. Construction rejects it.

## 5. FFI / shim boundary
- **Hand-written, reviewed once, about 300 lines:** `simjig_shim.h` and `simjig_shim.cpp`. They
  provide:
  - `VtbModel<V>`, a template wrapping any Verilated class;
  - the global `VL_USER_FATAL`, `VL_USER_STOP`, `VL_USER_STOP_MAYBE` and `VL_USER_FINISH`
    definitions;
  - a per-model message buffer;
  - `try/catch(...)` around every entry point.
- **Generated per model, about 50–200 lines:** the signal table (addresses of ports and
  `public_flat` members, taken once after construction) plus
  `extern "C" const VtbOps* simjig_ops_<prefix>(void)`.
- **Rust side, hand-written:** a single `#[repr(C)] struct VtbOps` with compile-time
  size/align asserts. The only *generated* Rust extern is the one `simjig_ops_<prefix>`
  declaration.

```c
typedef struct { void* ptr; uint32_t width; uint8_t storage; uint8_t access; } VtbSig;
typedef struct { uint8_t kind; uint32_t line; uint64_t time; char file[128], hier[256], msg[512]; } VtbMsg;
typedef struct VtbOps {
  uint32_t abi_version; uint64_t ir_hash;
  void*    (*create)(const char* name, uint32_t argc, const char** argv);
  void     (*destroy)(void*);
  int32_t  (*eval)(void*);                    /* 0 ok, 1 finish, 2 fatal, 3 c++ exception */
  void     (*set_time)(void*, uint64_t ps);
  int32_t  (*events_pending)(void*);  uint64_t (*next_time_slot)(void*);
  const VtbSig* (*signals)(void*, uint32_t* count);
  uint32_t (*take_msgs)(void*, VtbMsg* out, uint32_t cap);
  int32_t  (*trace_open)(void*, const char* path); void (*trace_dump)(void*, uint64_t); void (*trace_close)(void*);
  uint32_t (*coverage)(void*, uint32_t** counters); int32_t (*coverage_write)(void*, const char* path);
} VtbOps;
```

**Fatal routing.**
- `fatalOnError(false)` is set at construction.
- `vl_stop`/`vl_stop_maybe` record a `VtbMsg` and return.
- `vl_fatal` (which also covers Verilator's internal fatals) records a message, then
  **throws `VtbFatal`**. The `eval` wrapper catches it, returns 2 and marks the model
  *poisoned*: after that, only trace close and destroy are allowed.

Unwinding through Verilator code (not `longjmp`) runs C++ destructors correctly. M0 must
prove this against the pinned build. **Fallback** if exceptions are unavailable: the hook
writes the message to `target/simjig/.../fatal.msg`, then `_exit(70)`, and the harness
parent reports it.

DPI imports implemented in Rust are `extern "C"` functions that also go through
`catch_unwind`. A panic there is recorded and the kernel raises it after the eval.

**Rust tests drive a fake model through the same ABI.** `simjig::model::fake` builds a
`VtbOps` from Rust `extern "C"` functions over a small Rust state machine. The kernel's
tests therefore exercise the exact production FFI path, under Miri, with no trait
abstraction.

## 6. Codegen and the type IR

```rust
pub struct Ir {
    pub schema: u32,                 // IR_SCHEMA, bumped on any change
    pub verilator: String,           // "5.053-devel" exactly as reported
    pub top: ModuleId,
    pub types: Vec<Type>,            // interned and structurally deduplicated
    pub modules: Vec<Module>,
    pub interfaces: Vec<Interface>,
}
pub struct TypeId(u32);
pub enum Type {
    Bits { width: u32, signed: bool, four_state: bool },
    Struct { name: Name, packed: bool, fields: Vec<Field> },  // Field { name, ty, lsb: u32 }
    Union  { name: Name, fields: Vec<Field> },
    Enum   { name: Name, base: TypeId, variants: Vec<(String, Lit)> },
    Array  { elem: TypeId, msb: i64, lsb: i64, packed: bool },
}
pub struct Name { pub orig: String, pub verilator: String }  // origName vs specialised clone
pub struct Module { name: Name, params: Vec<Param>, ports: Vec<Port>, internals: Vec<Internal> }
pub struct Port { name: String, dir: Dir, ty: TypeId, member: String }   // C++ member name
pub struct Internal { path: String, ty: TypeId, access: Access, member: String }
pub struct Lit { pub width: u32, pub words: Vec<u32> }   // parsed Verilog literal
```

Width is a **derived, asserted** property: `Ir::width(TypeId)` is computed, never
stored. A layout-check SV module, generated under the same flags, gives every port type an
`initial` block that `$fatal`s if `$bits(T)` differs from the IR width. A C++ check
asserts that the storage class matches `sizeof(member)`. Both run in M0 and in every
fixture test.

**Versioning against Verilator.**
- Exactly **one pinned Verilator version** is supported. `xtask` provisions it. CI also
  runs a "next" job against master, which may fail without blocking.
- The JSON decoding lives in `svtypes/src/frontend/v5.rs`, the single file that knows the
  Verilator format. The raw structs are separate from `Ir`.
- Fixtures: about 40 SV snippets, with the JSON and an `Ir` snapshot (insta) per
  supported version.
- A Verilator upgrade is one PR: new fixtures, adjust `v5.rs`, and a reviewed snapshot
  diff.
- The generated code embeds `ir_hash` and `abi_version`. `Model::new` checks them against
  the ops table, so a stale cache gives an error, never UB.

Emitters write text through a 60-line indenting writer, not `quote`/`prettyplease`. That
keeps build scripts fast and the output byte-stable. Generated types get plain impls of
`Packed` (pack/unpack), `Arbitrary`, `Debug`/`Display` (enum names, field-wise struct
diff) and `Bundle`, with no proc macros on generated code. The Python back end emits
`dataclass`/`IntEnum` with `pack`/`unpack` for cocotb.

## 7. Errors, debuggability, observability
- **Failures are data:** `enum Failure`. Its variants are `Check` (field-level diff),
  `DutFatal`/`DutError` (file, line, hier, msg, time), `TaskPanic`, `DeltaLimit`,
  `Deadlock`, `Timeout`, `EndOfTest` (non-empty scoreboard, outstanding IDs, open
  objections), `QueueFull`, `ConstraintUnsat` and `ModelPoisoned`. Every `Display` says
  what happened, where (sim time, task path, signal) and what to do next.
- By default the test stops at the first failure (`max_failures = 1`), so the trace ends
  at the cause.
- **Flight recorder:** a fixed ring of 64 Ki 32-byte records:
  - kernel events (time, phase, delta, task, wait, signal, value-low-64);
  - user `simjig::log!` lines, interned.

  It has no global subscriber and no allocation per event. It is dumped on failure. Its
  trace hash is the determinism oracle.
- **Repro line on every failure:**
  `SIMJIG_SEED=0x9e3779b9 SIMJIG_CHOICES=target/simjig/fifo_in_order/9e37/choices.hex SIMJIG_WAVES=1 cargo nextest run -p fifo-tb --exact fifo_in_order`.
- **Waves:** set `SIMJIG_WAVES=1` to re-run deterministically with FST on. The
  `--trace-fst` support is always compiled in and stays off until opened. To make waves
  cheap on long tests, `SIMJIG_WAVES_FROM=<ps>` starts dumping late, or resumes from a fork
  checkpoint.
- **Triage:** failures are bucketed by signature (variant + location + DUT message, with
  numbers stripped), and the summary prints counts per bucket. JUnit comes from nextest.

## 8. Testing the framework
- **`simjig-bits`:** proptest against a `Vec<bool>` reference model, plus Miri.
- **`svtypes`:** fixture snapshots, the layout check against real Verilator, and literal
  and width proptests (generated SV types round-tripped through Verilator `$bits`).
- **Kernel:** deterministic simulation testing on the fake model. A *swarm* generator
  builds random task programs (spawn, await edge/time/event, write, cancel, panic) over
  random clock sets. After each run it asserts:
  - the slab is empty;
  - every trigger is deregistered;
  - time is monotonic;
  - the same seed gives the same trace hash.

  It runs under Miri for the unsafe modules.
- **Conformance:** about 30 toy DUTs, comparing (time, trigger, value) logs against
  pinned cocotb + Verilator.
- **Shim:** an ASan/UBSan build of shim plus model in CI. Fatal routing is tested with
  `$fatal`, `$stop`, an out-of-range internal fatal and a C++ exception.
- **VIP:** loopback against its own monitors, plus a reference RTL (verilog-axis, MIT).
- **Generated code and errors:** insta snapshots, and trybuild for the macro errors.

## 9. Extension points
- **4-state:** the IR already records `four_state`. Adding it means a new storage class
  in `VtbSig` (value plane plus X/Z plane via shim `get_planes`/`set_planes`), a
  `Logic<N>` value, and a second generated view (`SeqCmd4`). Kernel and handles are
  unchanged.
- **AXI4 / APB:** new modules in `simjig-vip`.
- **SystemRDL RAL:** a new `svtypes`-style front end emitting a register IR, plus
  `simjig-ral` using frontdoor (any bus) and backdoor (`InternalRw`) access.
- **Properties project:** consumes `Wait`, the ReadOnly phase, recorder events, `Failure`
  and coverage counters. These are all public data types.
- **Hierarchical SoCs:** one `Model` per opaque block, each with its own prefix, in one
  `Sim`. The multi-model path from lockstep is already there.

## 10. Key type sketches

```rust
pub struct SimTime(u64);                       // picoseconds
pub struct TaskId { index: u32, gen: u32 }
#[derive(Clone, Copy)] pub struct Sig<T, A> { model: ModelId, epoch: u32, index: u32, _p: PhantomData<(T, A)> }
pub trait Packed: Sized { const WIDTH: u32; fn pack(&self, out: &mut [u32]); fn unpack(words: &[u32]) -> Result<Self, Decode>; }
impl<T: Packed, A: Readable> Sig<T, A> { pub fn get(self) -> T; pub fn get_raw(self) -> BitsDyn;
    pub fn rising(self) -> Trigger; pub fn falling(self) -> Trigger; pub fn changed(self) -> Trigger; }
impl<T: Packed, A: Writable> Sig<T, A> { pub fn set(self, v: T); }

pub struct Choices { stream: StreamId }        // named sub-stream, Copy
pub trait Arbitrary: Sized { fn draw(c: Choices) -> Self; }   // smaller choice = simpler value
impl Choices { pub fn draw<T: Arbitrary>(self) -> T; pub fn range(self, lo: u64, hi: u64) -> u64;
    pub fn draw_where<T: Arbitrary>(self, ok: impl Fn(&T) -> bool) -> T; } // MAX_REJECT = 1000

#[simjig::test(cases = 256, clock(clk, ns = 10))]
async fn fifo_in_order(dut: fifo::Dut) {
    let stim: Vec<fifo::Word> = simjig::choices("stim").vec(0..=64);
    let mut sb = simjig::check::InOrder::<fifo::Word>::new("sb", 128);
    simjig::spawn("drv", vip::valid_ready::drive(dut.in_, stim.clone(), simjig::choices("bp")));
    simjig::spawn("mon", vip::valid_ready::monitor(dut.out, sb.actual()));
    stim.into_iter().for_each(|w| sb.expect(w));
    sb.drained(simjig::Timeout::cycles(dut.clk, 1000)).await;   // end-of-test check
}
```

**Randomness, shrinking and fuzzing share one concept.** Every random draw reads a
recorded **choice stream**. There are named sub-streams (stimulus `stim` and policy `bp`)
so the policy draws don't shift when stimulus changes.
- Seeded runs fill the streams from a xoshiro256** PRNG (about 30 lines, no dep).
- Shrinking edits the recorded streams (delete chunks, lower values toward zero, sort)
  and re-runs the whole deterministic test on a fresh model, Hypothesis-style.
- Fuzzing feeds libFuzzer bytes as the streams, and copies bucketed `__Vcoverage`
  counters into libFuzzer's extra-counters region.
- The minimal failing streams are saved to `tests/regressions/<test>.choices` and are
  replayed first on every run.

## 11. Top risks, and what I will not build
**Risks:**
1. **C++ exceptions through `eval`.** M0 must prove this against the pinned build. The
   fallback is record-then-`_exit`.
2. **The Verilator JSON format churns.** Mitigated by a single pinned version, one file
   that decodes the format, and fixture snapshots.
3. **Thread-local kernel lookup per signal access.** Measure it in M0. If it is above
   5 ns, the fix is cached base pointers inside `Dut`. The API does not change.
4. **`fork()` under the test harness.** Assert one OS thread, Verilator `--threads 1`,
   and no open trace files. It is Unix-only by design.
5. **Rust async ergonomics (`'static` spawns).** Addressed by `Copy` handles.

**Deliberately not built:**
- slang;
- `--savable`;
- tokio, proptest or tracing;
- UVM factory, config_db or phase objects (phases are ordinary async code: reset, run,
  `drained()`);
- `Driver`/`Monitor` traits and `#[derive(Env)]` until a second real use appears (VIP are
  concrete structs);
- a soft-cancel protocol;
- a mapping DSL;
- a constraint solver;
- multi-threaded models;
- a GUI.

## 12. Milestones
- **M0 (2 wks):**
  - `svtypes` v5 front end with width and literal handling, plus 20 fixtures;
  - the layout check;
  - emitters for Rust types, the ops table, `.vlt` and the wrapper;
  - `verilator --build` with the cache;
  - the hand-written shim with **fatal routing**, including the exception proof;
  - kernel with a single clock, `rising`, the settle loop, `spawn`, `catch_unwind`;
  - the fake model, and a FIFO test.

  Exit: zero hand-written types, the layout check passes, and `$fatal` becomes a
  `Failure` rather than killing the process.
- **M1 (5 wks):**
  - all four phases, write-conflict, deadlock and delta-limit failures;
  - sync primitives, structured `scope`/`join!`/`first!`;
  - recorder, report and repro;
  - choice streams, `Arbitrary` (generated and derived), `draw_where`;
  - scoreboards (in-order, out-of-order, by-ID), objections, end-of-test checks;
  - `simjig-vip` ValidReady and AXI-Stream;
  - the cocotb Python back end;
  - the conformance suite and the swarm tests.
- **M1.5 (2 wks):**
  - the shrinker (seeded FIFO bug shrinks to at most 3 transactions);
  - libFuzzer harness with coverage counters;
  - lockstep of old vs new RTL with a struct diff;
  - `fork()` checkpoints;
  - the pre-registered kill check.
- **M2 (5 wks):**
  - the multi-clock wheel and an async-FIFO CDC test;
  - `--timing` DUTs;
  - coverage (plain-data cover points; generated types supply enum and field bins; a
    `coverage.dat` writer, merged by `verilator_coverage`);
  - AXI4-Lite;
  - DPI stub models in Rust.

  Exit: an AXI-Lite register block reaches coverage closure, with a holes report.
