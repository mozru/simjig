# Proposal B: simjig architecture

Author: architect B (Rust library/ecosystem view). Verilator claims checked against the
5.053-devel tree under `review-verilator/` (`include/verilated.cpp:165-231`).

## 1. Guiding principles

1. **Two disjoint crate graphs: build time and run time.** Codegen never links into a test
   binary, and the runtime never parses JSON. Generated code is the only bridge and names
   only `::simjig::__private`. Editing either side never recompiles the other.
2. **One lowering, many emitters.** The IR is lowered once (names, storage classes, slot
   ids). The Rust, C++, `.vlt` and SV emitters all read that one value, so the Rust and
   C++ sides cannot drift.
3. **Writes are data.** Testbench writes go into a write log, not into model memory. That
   gives deterministic multi-task semantics (NBA/cocotb ReadWrite), conflict detection,
   `Drop` cleanup that can drive pins, and a trace of every stimulus bit.
4. **`unsafe` is a leaf.** Raw pointers live only in `simjig-sys` and
   `simjig-runtime/src/model/raw.rs`. The executor is safe Rust (`std::task::Wake`).
5. **Every failure is replayable.** Seeds plus a re-exec harness make "same seed, same
   trace" the debugger, and waves come from the replay.

I optimise for: semver-stable public surface (`simjig` facade + generated code) >
deterministic semantics > build latency > runtime speed.

## 2. Crate layout

```
BUILD TIME                                   RUN TIME
simjig-ir  (leaf: schema, serde)                simjig-bits (leaf)
  ^         ^            ^                   simjig-exec (leaf: executor, time, sync)
  |         |            |                   simjig-sys  (leaf: C ABI, C++ runtime shim, links=)
simjig-verilator-json   simjig-codegen  simjig-codegen-cocotb      ^        ^        ^
  ^   ^                ^   ^          ^                   +--- simjig-runtime -+
  |   +---- svtypes (bin: dump IR, gen Python) ----+           ^       ^
  +-------- simjig-build (build.rs library) ---------+         simjig-verif  |
                                                              ^        |
simjig-macros -> simjig-macros-impl (syn; no workspace deps)     simjig-vip  simjig-pbt
                                                              ^    ^
                                           simjig (facade: re-exports, #[simjig::test] harness,
                                                __private for generated code)
```

| Crate | Why it is its own boundary |
|---|---|
| `simjig-ir` | Front-end-agnostic schema with its own `SchemaVersion` semver. |
| `simjig-verilator-json` | The only crate that knows Verilator's JSON, so per-release churn and fixtures stay here. |
| `simjig-codegen` | Pure `Design -> Lowered -> {rs, cpp, vlt, sv}`. No I/O, so snapshots need no Verilator. |
| `simjig-codegen-cocotb` | A Python audience and cadence, kept off the Rust path. |
| `svtypes` | The standalone CLI. It ships even if the runtime is killed at M1.5. |
| `simjig-build` | build.rs orchestration: Verilator, dep file, header cross-check, `cc`. |
| `simjig-sys` | `links = "simjig_verilated"`. Builds libverilated plus `simjig_rt.cpp` **once per workspace** with the hook defines, exports `DEP_SIMJIG_VERILATED_*` to `simjig-build`, and holds the `#[repr(C)]` types. |
| `simjig-exec` | Executor, `SimTime`, timer wheel, `Event`/`Queue`/`Lock`. No Verilator, so it is Miri-able and reusable by the assertions project. |
| `simjig-runtime` | The kernel: timestamp loop, models, handles, clocks, failure routing, FST, coverage, `fork()`. |
| `simjig-verif` | Env/phases, objections, scoreboards, covergroups, `Randomize` runtime, `SeedTree`. |
| `simjig-vip` | ValidReady, AXI-S, AXI4-Lite. Highest churn, so its own semver. |
| `simjig-pbt` | Shrink, lockstep, fuzz. Heavy deps (`proptest`, `libfuzzer-sys`) stay opt-in. |
| `simjig-macros(-impl)` | A thin shell, with the logic in a normal crate tested with `insta` and `trybuild`. |
| `simjig` | The facade: one public path per item. |

Invariants (go in ARCHITECTURE.md): the build-time and run-time graphs are disjoint. Only
`simjig-sys` and `simjig-runtime::model::raw` may contain `unsafe`, and everything else has
`#![forbid(unsafe_code)]`. Generated code names only `::simjig::__private::*`. This
**amends** the current invariant "only `simjig` has unsafe": the C ABI deserves its own
reviewed leaf.

## 3. Data flow

```
rtl/*.sv + build.rs config (top, -G params, public_rd/rw list, iface-port params, timing)
 1 simjig-build: [optional] gen wrapper SV for interface top ports
 2 verilator --json-only --no-timing -G.. -D..   -> design.tree.json  (+ --MMD deps)
 3 simjig-verilator-json: adapter(version) -> simjig_ir::Design            (widths computed here)
 4 simjig-codegen: lower(Design, BuildCfg) -> Lowered
        emit_rust  -> OUT_DIR/simjig/<dut>.rs     (types, Dut, signal table, metadata)
        emit_cpp   -> OUT_DIR/simjig/<prefix>_shim.cpp (vtable, pointer table, static_asserts)
        emit_vlt   -> OUT_DIR/simjig/<dut>.vlt     (public_flat_rd/rw for listed signals only)
 5 verilator --cc --prefix V<dut>_<paramhash> [--timing] --coverage? --trace-fst
        --output-split + *.vlt   -> C++
 6 cross-check: parse V<prefix>.h VL_IN/OUT(&p,msb,lsb) lines vs IR port widths -> hard error
 7 cc (parallel) model + shim against simjig-sys's include/defines; link static
 8 test binary: simjig::include_dut!(fifo) -> Dut handle; kernel runs
 9 outputs: target/simjig/<test>/{events.jsonl, summary.json, coverage.dat, waves.fst on replay}
          + JUnit via nextest
```

Step 6 checks every port, on every build, against a second source Verilator emits itself:
the cheapest drift detector there is. The SV layout-check module (`$bits(T)` for every
type) runs in CI.

**Interface top ports.** An interface port's parameters come from its instantiator, and
the testbench *is* the instantiator. So `build.rs` names them
(`.iface_port("s_axis", "axis_if #(.W(32))")`). The RTL doesn't contain this
information, so single source of truth still holds. The wrapper flattens the port and the
IR records the mapping, so `dut.s_axis` is a bundle.

**Rebuilds.** `rerun-if-changed` comes from Verilator's `.d` file, so it is exact.
`libverilated` builds once in `simjig-sys`. An opt-in content-addressed `SIMJIG_CACHE_DIR`
serves CI.

## 4. Core runtime design

**Ownership.** `Sim` owns a `Kernel`, which owns `Vec<Model>`, each with its own
`VerilatedContext` and `--prefix`. The kernel is `!Send`. During `Sim::run` it is
installed in a thread-local (the `tokio::runtime::Handle::current` pattern).

**Signal handles are `Copy`** values (`{ sim: SimGen(u32), slot: SlotId(u32) }`) resolved
through that TLS. I rejected lifetime-branded `Signal<'d>`: it forces scoped non-`'static`
tasks and a lifetime on every VIP struct, and async lifetime errors are the worst
diagnostics for agents. I rejected `Rc` handles because they are not `Copy`. Each access
checks the generation (a stale handle panics, naming the signal) and the slot bounds.
Then `raw.rs` does one `ptr::read` of the member's storage class. No Rust reference to
model memory ever exists, and no task code runs during `eval()`. A DPI callback that
touches a handle hits an `in_eval` flag and gets a clear panic.

**Direction is a type parameter.** `In` has `write`, `Out` and `PublicRd` only `read`, and
`PublicRw` has `deposit`. Codegen emits `PublicRw` only for signals the `.vlt` lists, so
"writing non-public internals" is unrepresentable.

**Writes.** `write` masks to `T::WIDTH` and appends `(slot, raw, task)` to the write log,
which is applied just before the next `eval()`. Two tasks writing different values to one
slot in one batch give `Failure::WriteConflict` naming both spawn sites. A write in
ReadOnly is always `Failure::WriteInReadOnly`, because it is a single flag check. Reads
show the design's current value. This matches cocotb's ReadWrite-applied writes, and the
conformance suite pins it.

**Timestamp loop** (normative, `simjig-runtime/src/kernel/step.rs`):
```
t = min(timer_wheel.head, clocks.next_edge, models.next_time_slot())  // eventsPending() checked first
for m in models: m.ctx.time(t)
log.extend(clock edges due at t, declaration order); wake timers due at t
loop delta in 0..=limit:
    apply write log; eval each dirty/edged/timing-due model; drain shim failures
    diff watched slots (only slots with waiters) -> wake waiters in registration order
    run ready tasks FIFO to quiescence
    if log.is_empty(): break
  else Failure::DeltaLimit { slots_written_last_delta, writers }
phase = ReadOnly; wake ReadOnly waiters; run; trace.dump(t); phase = Active
if nothing scheduled && main task pending -> Failure::Starved { waiting tasks + Display of each trigger }
```

**Time.** `SimTime(u64)` counts ticks of the design's `timeprecision` (from the IR, 1 ps
by default). `ns(10)`-style constructors are checked: a period that can't be represented
is an error at `clock()`, not a rounded value. The wheel only pops times ≥ now, so time
is monotonic.

**Multi-clock.** Clocks are kernel-native wheel entries, not tasks
(`sim.clock(dut.clk, ns(10)).phase(ps(2500)).start()`). Edges at the same time coalesce
into one eval, so CDC with coprime periods just works. Triggers on internal derived clocks
need `PublicRd`, are documented as *sampled at eval boundaries* and are tagged `sampled`
in the trace.

**`--timing` DUTs.** The model is built with `--timing`, while the JSON pass always uses
`--no-timing`. The shim calls `nextTimeSlot` only after `eventsPending` returns true,
because it aborts otherwise. A model with timing events due is evaluated even if the
testbench wrote nothing.

**Waker model.** `std::task::Wake` on `Arc<TaskWaker { id, queue }>`. The queue is an
uncontended `Mutex<VecDeque<TaskId>>` plus a thread-id assert. This is the one sanctioned
`Arc`/`Mutex`, because anything reachable from a waker must be `Send + Sync`. Triggers
register `cx.waker()` per slot and deregister in `Drop`. `TaskId`s are monotonic, the run
queue is FIFO, and there is no `HashMap` on any scheduling path.

**Tasks and cancellation.** `spawn(fut) -> Task<T>`, and `join().await` returns
`Result<T, TaskError>`. There is **one** cancellation mechanism: structured drop, where a
scope cancels its children. Writes are synchronous log entries, so `Drop` guards can
deassert `valid` or release a `Lock`. A graceful stop (finishing a handshake) is a `Stop`
token the component selects on, not executor semantics. Soft `Cancelled`-at-next-await is
rejected: every trigger would have to return `Result`, and it adds a second semantics.
Each poll runs under `catch_unwind`, and a panic becomes `Failure::TaskPanic`.

**End of test.** When the main future returns, the kernel runs the `on_end` checks
(scoreboards empty, no outstanding IDs, objections zero), drops all tasks, calls `final()`
and writes the summary. `$finish` ends the run, and the checks still run.

## 5. FFI/shim boundary

**Hand-written** (`simjig-sys/cpp/simjig_rt.{h,cpp}`, ~300 lines, the only C++ we maintain):
- The hook defines `VL_USER_FATAL`, `VL_USER_STOP`, `VL_USER_FINISH`, `VL_USER_WARN`, plus
  `-include simjig_config.h` (which redefines `VL_PRINTF`). They apply to libverilated *and*
  to every model translation unit, through `DEP_SIMJIG_VERILATED_DEFINES`.
- `vl_fatal`/`vl_stop` record `{file, line, hier, msg, kind}` in the context's failure ring
  and then `throw simjig::FatalUnwind`. Generated Verilator code has no `noexcept` and
  `vl_fatal` is not `[[noreturn]]`, so unwinding through it is legal. After that the
  model is marked **poisoned**: no further `eval`, and only reporting is allowed.
- `vl_stop_maybe` below the error limit and `$error` record without throwing.
- `SIMJIG_TRY` macro: every `extern "C"` entry point catches all C++ exceptions and returns
  `SimjigStatus`.

**Generated per model** (`<prefix>_shim.cpp`): a single exported symbol,
`const SimjigModelVTable* simjig_vtable_<prefix>(void)`. It covers create/destroy/eval/final,
`events_pending`/`next_time_slot`, `slot_ptrs(h, out, n)` (member addresses, including
`rootp->x__DOT__y` for public signals), trace open/dump/close, `coverage(h, &len)` (the
`__Vcoverage` array) and `rand_seed`. It also emits `static_assert(sizeof(member) ==
storage_bytes)` for every slot. DPI imports implemented in Rust get a generated C++
trampoline: it calls a Rust `extern "C"` function that returns a status, so a Rust panic
is caught on the Rust side and rethrown as `FatalUnwind` in C++. A panic never crosses
FFI.

**C ABI rule:** the vtable starts with `abi_version: u32` and `size: u32`. Entries are
append-only, so a 4-state `get_planes`/`set_planes` can be added later without breaking
older generated code. Generated Rust contains exactly one
`unsafe extern "C" { safe fn simjig_vtable_<prefix>() -> &'static SimjigModelVTable; }`, and
that block is snapshot-reviewed. Static linking only: I reject Marlin's runtime
`dlopen`, because it bypasses cargo's fingerprinting.

## 6. Codegen and the IR schema

```rust
pub struct Design { pub schema: SchemaVersion, pub provenance: Provenance /*tool, version, flags hash*/,
    pub timescale: Timescale, pub types: Vec<Type> /*arena, TypeId = index*/, pub top: Module }
pub struct Type { pub name: Option<QualName>, pub kind: TypeKind, pub width: BitWidth, pub four_state: bool }
pub enum TypeKind {
    Bits { signed: Signedness },
    Struct { packed: Packing, fields: Vec<Field> },   // Field { name, ty, lsb: u32 } (lsb always present)
    Union  { packed: Packing, fields: Vec<Field> },
    Enum   { base: TypeId, variants: Vec<Variant> },  // Variant { name, value: Literal }
    Array  { elem: TypeId, dims: Vec<Range>, packed: Packing },
}
pub struct Module { pub name: Ident /*origName*/, pub params: Vec<Param>, pub ports: Vec<Port>,
    pub bundles: Vec<BundleMap> /*flattened iface ports*/, pub public: Vec<PublicSignal> /*hier path, access*/ }
pub struct Literal { pub width: BitWidth, pub signed: Signedness, pub hex: String } // parsed, canonical
```

- **The front end normalises.** It computes widths, turns a missing `lsb` into 0, parses
  literals (X/Z rejected unless `four_state`) and dedupes specialised clones by `origName`
  plus a structural hash. The IR never says "Verilator".
- **Lowering** (`lower.rs`) makes every naming decision: Rust idents (`r#type`,
  collisions), storage class (`u8..u64`/`[u32; W]`, matching CData..VlWide), `SlotId`s and
  access kind. `Lowered` is snapshot-tested on its own.
- **Generated shape.** Packed structs become `#[repr(transparent)]` over `Bits<N>`, with
  field accessors. Enums become **newtypes with associated consts** (`State::IDLE`) plus
  `variant() -> Result<StateKind, InvalidEnum>`. SV enums can hold invalid encodings, so
  reads stay infallible and honest. Every type gets `Debug`, `Diff`, auto coverage bins and
  a proptest `strategy()`. Codegen fills in `W` for `WideBits<N, W>`.
- **Versioning.** Each Verilator release range gets an adapter (`adapters/v5_05x.rs`),
  chosen from `verilator --version`. It deserialises into a minimal dtype subset. Unknown
  node kinds become `Unknown { type_name }` and are an error only when a port or type
  reaches them, naming the node and version. Each fixture
  (`fixtures/verilator-<ver>/*.json`) is paired with an IR snapshot. An untested version
  fails with a hint (`SIMJIG_ALLOW_UNTESTED_VERILATOR=1`). A nightly job against master turns
  format drift into a fixture diff. `SchemaVersion` follows semver, and the cocotb back end
  rejects unknown major versions.

## 7. Errors, debuggability, observability

Build errors (`simjig_build::Error`, `thiserror`) carry the Verilator command line, the
stderr excerpt and the offending SV path, and are rendered by `anyhow` in build.rs.

Run time: `simjig::Result = Result<(), Report>`. A `Report` holds a `Vec<Failure>` plus the
repro.

```rust
pub struct Failure { pub kind: FailureKind, pub time: SimTime, pub task: Option<TaskName>,
                     pub component: ComponentPath, pub location: Option<Location> }
pub enum FailureKind { DutFatal{file,line,hier,msg}, DutError{..}, TaskPanic{msg, spawned_at},
  Check{expected, actual, diff}, DeltaLimit{..}, WriteConflict{..}, WriteInReadOnly{slot},
  Starved{waiting: Vec<(TaskName, String)>}, Timeout{..}, EndOfTest{..}, StaleHandle{..} }
```

Triage chain:
- **Failure.** Every report ends with a pasteable repro:
  `SIMJIG_SEED=0x9f2e… cargo nextest run -p fifo-tb -E 'test(=overflow)'`.
- **Triage.** Failures are bucketed by `(kind, location, component)` in `summary.json`.
  `events.jsonl` records spawns, wakes with their trigger, every write-log entry, DUT
  prints and failures. `tracing` spans carry sim time and component path.
- **Repro.** Determinism comes from `SeedTree`: each named component stream is derived
  from `hash(root, path)`, so adding a component does not perturb the others. Verilator's
  `rand_seed` comes from the root seed too.
- **Waves.** With `SIMJIG_WAVES=on-failure`, the harness re-execs `current_exe()` with the
  same seed and `SIMJIG_WAVES_FROM = t_fail - window`, then prints the FST path. No run pays
  for tracing up front, and the replay doubles as a determinism check: if the replay does
  not reproduce the failure, it says so loudly.

## 8. Testing the framework

1. **Tier 0, no Verilator, under 60 s, every commit:**
   - `simjig-bits` proptests against a `BigUint` reference.
   - Fixture-to-IR snapshots and `Lowered`/`.rs`/`.cpp`/`.vlt` snapshots.
   - `trybuild` UI tests, including misuse such as writing an `Out`.
   - The executor under Miri.
   - **Kernel conformance against `MockModel`**, the second implementation of the
     internal `ModelBackend` trait, with closures as logic. It is the one trait that earns
     its keep.
2. **Tier 1, pinned Verilator:** `tests/duts/*` (FIFO, struct pipe, comb chain, async
   FIFO, `--timing`, `$fatal`, DPI panic, interface top), with `insta` snapshots of
   `(time, trigger, value)` logs.
3. **Tier 2, nightly:**
   - The cocotb differential suite (about 30 DUTs) and the generated Python under cocotb.
   - Fixtures from Verilator master.
   - ASan/UBSan.
   - `cargo semver-checks` and `cargo public-api` snapshots for `simjig`, `simjig-ir` and
     `simjig-vip`.

## 9. Extension points

| Future | How it lands |
|---|---|
| 4-state | `Type.four_state` already exists. Codegen emits a `dut4` module with `Logic` views, the vtable appends `get_planes`/`set_planes`, and a `Packed4` trait sits beside `Packed`. The core is not made generic. |
| AXI4 full, APB | New modules in `simjig-vip` on the same `Bundle`/`Driver`/`Monitor` traits. |
| SystemRDL RAL | A new build-time crate, `simjig-rdl`. It emits Rust against a `RegBus` trait (implemented by the AXI-Lite master), and feeds `hdl_path` backdoors into the existing `.vlt` emitter. |
| Assertions project | Depends on `simjig-exec` and the stable `simjig::hooks` (ReadOnly observers, `Failure` sink, coverage merge). No simjig change is needed. |
| Hierarchical SoC | `Module.opaque`, with multiple models per kernel (already needed for lockstep). |
| slang front end | A second producer of `simjig_ir::Design`. Its cross-check is an IR diff. |

## 10. Key type and trait sketches

```rust
// simjig-bits
pub struct Bits<const N: u32>(u128);                 // N <= 128, enforced by inline const assert
pub struct WideBits<const N: u32, const W: usize>([u32; W]); // VlWide layout; W == N.div_ceil(32) asserted
pub trait Packed: Copy + 'static { const WIDTH: u32; type Raw: Storage;
    fn to_raw(self) -> Self::Raw; /* masked */ fn from_raw(raw: Self::Raw) -> Self; }

// simjig-runtime
pub struct In; pub struct Out; pub struct PublicRd; pub struct PublicRw;
#[derive(Clone, Copy)] pub struct Signal<T: Packed, D> { sim: SimGen, slot: SlotId, _p: PhantomData<(T, D)> }
impl<T: Packed, D: Readable> Signal<T, D> { pub fn read(self) -> T; pub fn rising(self) -> Edge; pub fn changed(self) -> Changed<T>; }
impl<T: Packed> Signal<T, In> { #[track_caller] pub fn write(self, v: T); }
pub(crate) trait ModelBackend { fn eval(&mut self) -> Result<(), ShimFailure>;
    fn next_time_slot(&self) -> Option<SimTime>; fn slot_ptr(&self, s: SlotId) -> NonNull<u8>; }

// simjig-sys
#[repr(C)] pub struct SimjigModelVTable { pub abi_version: u32, pub size: u32,
    pub create: unsafe extern "C" fn(*mut SimjigContext) -> *mut SimjigModel,
    pub eval: unsafe extern "C" fn(*mut SimjigModel) -> SimjigStatus, /* … append-only */ }

// simjig-verif / simjig-vip
pub trait Driver  { type Item; async fn send(&mut self, item: Self::Item); }
pub trait Monitor { type Item; fn subscribe(&self) -> Queue<(SimTime, Self::Item)>; }

// user code
simjig::include_dut!(fifo);
#[simjig::test]
async fn overflow(sim: &simjig::Sim, dut: fifo::Dut) -> simjig::Result {
    sim.clock(dut.clk, ns(10)).start();
    let mut tx = AxisDriver::new(dut.s_axis, dut.clk, sim.rng("tx.backpressure"));
    let rx = AxisMonitor::new(dut.m_axis.monitor(), dut.clk);
    let sb = sim.scoreboard::<fifo::Word>("sb").in_order();
    for w in sim.rng("stim").sample_vec(fifo::Word::strategy(), 64) { sb.expect(w); tx.send(w).await; }
    sb.drain_from(rx.subscribe()).await;
    Ok(())
}
```

## 11. Top risks and what I would not build

Risks:
1. **Verilator JSON drift.** Mitigated by adapters, fixtures, the header cross-check on
   every build and nightly runs against master.
2. **Unwinding from `vl_fatal`** through generated code, especially `--timing`
   coroutines. Mitigations: poison the model, Tier-1 tests for a fatal inside a coroutine,
   and a fallback that records the failure, returns and checks `gotError()` after `eval`.
3. **C++ compile latency.** Mitigated by `--output-split`, parallel `cc`, a shared
   `libverilated` and `SIMJIG_CACHE_DIR`.
4. **Write-log surprise** (reading right after your own write shows the old value).
   Documented on one page, pinned by conformance tests, and the same as cocotb.
5. **Lockstep and `$urandom`.** The RNG is thread-local, so it is shared between models
   in one thread. Lockstep rejects compared models that use `$urandom`, found by an IR
   scan, rather than letting them silently diverge.

Deliberately NOT built:
- A second cancellation kind.
- `simjig.toml`.
- `dyn Driver` registries or a UVM factory, config_db or phase graph.
- A multi-threaded executor.
- VPI.
- `--savable`.
- Runtime `dlopen`.
- A wave viewer.
- A solver.
- A Rust property language.
- A generic N-state core.

**Marlin.** Its `LICENSE` and `Cargo.toml` say **MPL-2.0, not MIT** as the brief states.
Its runtime build plus `dlopen` and its FFI call per port access also differ from this
design. Borrow ideas, not code.

**Fix the agentic-rust skill.** Its §9a ("drive on falling edge, 2 evals, no `--timing`",
slang primary) contradicts the brief. Fix it in M0.

## 12. Milestones

| Milestone | Deliverables |
|---|---|
| **M0** | `simjig-ir`; `simjig-verilator-json` (one pinned version, fixtures); `simjig-codegen` Rust/C++/`.vlt` with snapshots; `simjig-sys` (libverilated plus fatal hooks); `simjig-build` with the header cross-check; `simjig-exec`; a minimal `simjig-runtime` (one clock, write log, settle loop, `MockModel`); FIFO test. Exit: zero hand-written types, layout check green, `$fatal` reported and not `exit(1)`. |
| **M1** | The full kernel loop and conformance suite; `Starved`/`DeltaLimit`/`WriteConflict`; `simjig-verif` (scoreboard, quiesce, end-of-test checks, `Randomize` derive, `SeedTree`); `simjig-vip` ValidReady and AXI-S with `Bundle` derive; `events.jsonl` and repro lines; `simjig-codegen-cocotb` plus `svtypes` CLI. |
| **M1.5** | `simjig-pbt`: shrinking over stimulus data, `fork()` checkpoints (Unix, `--threads 1`), the libFuzzer harness over `__Vcoverage`, lockstep with multiple models per kernel; waves on failure via re-exec. Then the kill check. |
| **M2** | Multi-clock with phases and an async-FIFO CDC test; `--timing` models in `next_time`; covergroups with Verilator `.dat` merge and holes report; AXI4-Lite; DPI trampolines for Rust models; interface-top wrapper. |
