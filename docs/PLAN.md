# simjig: Rust testbenches that drive Verilator (plan v3)

> **Scope update (supersedes §6 and the VIP/methodology items in §4, §7 and §8):** simjig
> covers only the Verilator-Rust interface. VIP, AXI, scoreboards, constrained random,
> coverage closure, RAL, shrinking and fuzzing harnesses are out of scope.
> See `docs/architecture/DECISION.md`.

v3 revises v2 after four adversarial reviews (Verilator internals, design/Rust, prior art,
verification methodology). Verilator claims are checked against Verilator master
(5.053-devel); the reviewers' citations are kept as `path:line`. `simjig` is a working name.

## 1. Vision and positioning
A general-purpose framework for testing any RTL with Verilator: custom interfaces,
standard buses, block-level to SoC-level tests. zstd is one flagship example.

**Lead with the features that need the testbench in charge.** These are the
differentiators; the rest is table stakes:
- property-based testing that shrinks failing stimulus to a minimal case;
- differential/lockstep testing against a Rust reference model or a previous RTL revision;
- coverage-guided fuzzing;
- warm-start checkpoints;
- testbench types generated from your SystemVerilog.

Pitch: *"Property-based and differential testing for Verilator RTL, in Rust, with types
generated from your SV."* The architecture stays general, but we don't chase cocotbext or
pyuvm parity until real users ask for it.

## 2. Principles
1. **The testbench owns the main loop and time.** The Verilator model is a library the
   testbench advances. *Exception:* DPI-C callbacks are allowed for in-cycle behavioural
   models (stand-ins for vendor IP, combinational paths), because Verilator schedules
   them correctly. A testbench writing stub outputs between evals has sampling races.
2. **The RTL is the single source of truth.** Types, enums, params, ports, interfaces and
   names are generated, never written by hand. Registers come from SystemRDL.
3. **Tests read like SystemVerilog:** async/await triggers, fork/join, events, queues.
4. **Stimulus is data.** Sequences produce values (`Vec<Txn>` or a proptest `Strategy`)
   that drivers consume. Randomness for policies such as backpressure comes from separate
   seeded streams. Shrinking only works if this holds.
5. **Verilator-native, deterministic, cargo-native.** Pinned Verilator version,
   `--threads 1` by default, seeded everything, `BTreeMap` ordering, nextest.
6. **Generated types, not generic types.** Signals take their generated types. 4-state
   becomes an alternative generated view later; the core is not generic over the number
   of states.

## 3. Non-goals and known limits
- Other simulators, VHDL, a UVM port.
- Assertions/properties (SVA, bind checkers, a Rust property language): a separate project
  that sits on top of simjig's hooks.
- **Encrypted vendor IP** is impossible in Verilator. Verilator has no tested support for
  UNISIM/XPM/altera_mf (nothing in its test suite), so treat these as best-effort.
  We ship generated stubs and DPI-backed Rust behavioural models instead.
- **4-state:** `--fourstate` is undocumented and warns FUTURE (`src/V3Options.cpp:1061`);
  PR #7193 is still open, and it is rejected with `--hierarchical`. We keep the design
  ready (the IR records a 4-state flag; a `Logic` view type exists) but give it no
  milestone.
- **`--hierarchical` builds:** internals of a hierarchy block can't be probed, there are
  no modports at block boundaries, and type parameters aren't supported. Each block is
  opaque to the testbench.
- **Triggers on internal derived or gated clocks are approximate.** The testbench only
  sees values at eval boundaries, so a pulse inside one `eval()` is invisible.

## 4. Architecture: start with 5 crates; add more only when needed
| Crate | Role |
|---|---|
| `svtypes` | Type IR (JSON): types, widths, offsets, signedness, 4-state flag, enums, params, ports, interfaces/modports, hierarchy with **Verilator's own names**. **Primary front end: `verilator --json-only`**, the same tool, flags (`-G`/`-D`) and naming as the model build, run with `--no-timing` and keyed on `origName`. Our own width calculator and Verilog-literal parser, with golden fixtures per supported Verilator version. **Secondary front end: slang**, used for the cocotb Python back end and as a cross-check. A generated layout-check SV module covers every port type under the same flags. Back ends: Rust, plus **Python/cocotb in M1** (the part with the widest audience). |
| `simjig-bits` | `Bits<N>`/`SBits<N>` (u128 up to 128 bits, ruint limbs above), `sv!`/`cat!`, explicit `zext`/`sext`/`trunc`; `Logic<N>` view type (IEEE X semantics) behind a feature flag. |
| `simjig-build` | Codegen plus Verilator invocation from build.rs: the generated C++ shim (into OUT_DIR), the typed DUT, the `.vlt` file (`public_flat_rd/rw` only for referenced signals), the wrapper top for DUTs with interface ports, a content-hash cache, and a unique `--prefix` per variant. |
| `simjig` | Runtime: scheduler (§5), triggers, `Task`/`scope`, soft cancel, per-task panic capture, `Event`/`Lock`/`Queue`, `with_timeout`; signal handles (Rc-backed, generation-checked, `!Send`); error hooks; FST control. Re-exports macros. |
| `simjig-macros` | `#[simjig::test]`, `#[derive(Bundle)]`, `#[derive(Randomize)]`, `covergroup!`. |
| *later:* `simjig-std` (M1), `simjig-cover` (M2), `simjig-tools` (M1.5), `simjig-rdl` (M2) | Added when their milestone needs them. |

### Interfaces and bundles
```rust
#[derive(Bundle)] #[bundle(prefix = "{p}_")]
struct Stream<T: Packed> { #[sig("vld")] valid: Bit, #[sig("rdy")] ready: Bit, data: T }
trait Driver  { type Item; async fn send(&mut self, it: Self::Item); }
trait Monitor { type Item; fn subscribe(&self) -> Queue<(SimTime, Self::Item)>; }
```
- One struct gives two views: a driver view and a read-only monitor view. Modport
  directions decide which signals each can write.
- Bundles generated from SV interfaces and from naming conventions emit this same derive.
  There is no `simjig.toml` mapping language.
- Use generics or enum dispatch, not `Box<dyn Driver>` (async trait methods can't be
  boxed as trait objects).
- The wrapper rejects modport expressions with part-selects (`src/V3LinkDot.cpp:3100`) and
  flattens interface arrays and nested interfaces.

## 5. Scheduling semantics (normative spec + conformance suite)
The general loop runs **once per distinct timestamp** (the old "drive on the falling edge"
rule only holds for a single clock):
1. Apply every clock edge due at t, then `eval()`. Edges at the same time are applied
   together, in clock-declaration order.
2. Wake testbench waiters (`RisingEdge` and friends; flops already show their new values,
   as in cocotb on Verilator). They write inputs, then `eval()`. Repeat while the
   testbench keeps writing, up to a delta limit that fails loudly.
3. `ReadOnly`: monitors sample; writes are illegal (a debug-build panic).
4. Next time t' = min(time wheel, `nextTimeSlot()` if the DUT uses `--timing`). Set
   `contextp->time(t')`. Time never goes backwards.

Other rules:
- Clock periods are integer picoseconds.
- Triggers are allowed on testbench-driven clocks and on `public_flat_rd` signals that
  only change at eval boundaries.
- Writes are deposits (they last until the design drives the signal again).
- Internal writes are allowed only on signals the `.vlt` lists as `public_flat_rw`.
  `forceable` is used for real force, read through `__VforceRd`.
- **Conformance suite:** about 30 toy DUTs, comparing (time, trigger, value-seen) logs
  between simjig and a pinned cocotb + Verilator. About 1 week.

## 6. Verification methodology (forastero-style structure with UVM role names)
- **Roles:** Driver, Monitor, Agent (active/passive), Scoreboard, Env, wired by
  `#[derive(Env)]`. Components talk through channels, never shared `RefCell`s.
- **Phases:** build, reset, run, check. A drain time and an objection count
  (`tb.quiesce(n)`).
- **Mandatory end-of-test checks:** the scoreboard is empty and no transaction IDs are
  outstanding.
- **Constrained random:** `#[derive(Randomize)]` with `#[range]`, `#[dist]` and
  `#[constraint]`, plus per-call `.with(|t| ..)`. Uses rejection sampling with proptest
  `Strategy` composition, so stimulus can always be shrunk. Leaves a hook for a solver
  backend.
- **Sequences:** async streams feeding drivers. Layering is done by adapter tasks; virtual
  sequences are plain async fns; responses are routed back by ID (AXI).
- **Scoreboards:** in-order, out-of-order and multi-stream; a predictor trait (a Rust model
  or a lockstep model); match keys and tolerances; latency statistics. Mismatches show a
  field-level struct diff with enum names.
- **Functional coverage:** `covergroup!` with bins, crosses, transitions and illegal/ignore
  bins. Generated types give automatic bins for every enum variant and struct field.
  Results are merged across seeds into Verilator's coverage format, plus a holes report.
- **Assertions and properties are out of scope; they are a separate project.** That
  covers SVA, bind checker libraries, and a Rust temporal/property layer. simjig only
  provides the hooks such a project needs:
  - DUT `$error`/`$fatal`/`$stop` (whatever produced them) go through the
    `VL_USER_STOP_MAYBE`/`VL_USER_FATAL` overrides plus `VL_PRINTF` capture, and become
    `TestFailure { file, line, hier, msg, time }`. Without this, one failure kills the
    process, so it is required regardless.
  - A stable trigger and sampling API (§5) and the coverage merge point, which a property
    layer can build on.
- **Reset-aware components:** a reset cancels in-flight items, flushes scoreboards and
  re-arms monitors. **Error injection:** pre/post-drive callbacks.
- **Reporting:** `tracing` with sim-time and component-path spans, per-severity counts,
  max-errors-then-stop, and a summary.
- **Triage:** a one-line repro command on every failure, failure signatures grouped into
  buckets, JUnit output via nextest, and **waves on failure** (re-run from the warm-start
  checkpoint, or from scratch, with FST tracing on).
- **RAL:** SystemRDL through a PeakRDL Rust exporter, with frontdoor access over any bus
  adapter, backdoor access through `public_flat_rw`, and the standard register sequences.

## 7. Features that need the testbench in charge (honest scope)
| Feature | How | Limits |
|---|---|---|
| Shrinking | proptest over stimulus that is data; each attempt re-runs the model in-process | Needs principle 4 |
| Coverage-guided fuzzing | The shim exposes the `__Vcoverage` counter array plus `zero()` as an AFL-style bitmap, bucketed; cargo-fuzz | Line/branch coverage is preferred; toggle coverage is expensive on wide buses |
| Lockstep diff | Two models with distinct `--prefix`, one thread each; first divergent cycle reported with a struct diff | No DPI exports; `$urandom` streams must be per-thread |
| Warm-start checkpoints | **`fork()` server** with `--threads 1`: copy-on-write, and it captures testbench state too. `--savable` only for saving to disk across processes | `--savable` is incompatible with `--coverage` and `--timing` (`src/V3Options.cpp:1069-1073`) and saves no testbench futures, so checkpoint only at quiet points. Unix only. |
| Rust models for missing IP | DPI-C imports in a generated stub | Breaks "the simulator never calls the testbench", hence the exception in principle 1 |

## 8. Milestones
| Milestone | Length | Contents | Exit criteria |
|---|---|---|---|
| **M0** | 2 wks | `svtypes` (Verilator JSON) to Rust; layout check; shim; one clock; FIFO test | Zero hand-written types; the layout check passes |
| **M1** | 5 wks | Scheduler (§5) and conformance suite; Bundle/ValidReady/AXI-Stream; Randomize; sequences; scoreboard; quiesce and end-of-test checks; reporting; DUT `$error`/`$fatal` routing; nextest; **minimal cocotb Python back end** | Struct pipeline, width converter and AXI-S FIFO verified |
| **M1.5** | 2 wks | **Prove the testbench-in-charge features:** shrinking finds a minimal stimulus for a seeded FIFO bug; libFuzzer harness; lockstep of old vs new RTL; fork checkpoint | **Kill check (pre-registered):** simjig vs cocotb + svtypes on the same three designs, with the cocotb side written by an agent on the same budget. Metrics: edit-to-result latency (test change, RTL change), seeded bugs caught, time to debug a seeded failure, lines of code, wall-clock time. **Kill the runtime if simjig wins only on wall-clock time;** svtypes ships either way. |
| **M2** | 5 wks | Multi-clock wheel and async FIFO (CDC); `--timing` DUTs; covergroups and merge; AXI4-Lite plus a PeakRDL Rust RAL; DPI stub models | AXI-Lite register-block test with RAL and coverage closure report |
| **M3** | — | PicoRV32/Ibex testbench; zstd decoder flagship; waves-on-failure polish | Real designs tested end to end |
| **M4** | — | AXI4 full (port cocotbext-axi's algorithms, MIT licence; 4–8 wks), APB/Wishbone; 4-state view once Verilator merges it | Driven by real user demand |

## 9. Build vs contribute
- `svtypes`: build it, as a standalone tool with Rust and Python back ends.
- Runtime: **write it from scratch.** Marlin (MPL-2.0) is prior art only: its runtime
  `dlopen`, per-port FFI calls and synchronous API don't fit our design (static linking,
  one vtable per model, async kernel, generated types). Credit it; take no code. Borrow
  ideas, not code, from Hardcaml (step testbench, expect-style waveform tests).
- Bus models: port cocotbext-axi when needed.

## 10. Open questions
- Scripting route for engineers who don't know Rust: is the cocotb back end of svtypes
  enough?
- Solver backend for constrained random (bitwuzla/z3): needed in v1, or never?
