# Architecture brief: simjig

## What we are building
simjig is a general-purpose framework for verifying any RTL design with Verilator, in Rust:
custom interfaces, standard buses (AXI), block-level to SoC-level tests.

- **Core principle: the testbench owns the main loop and time.** The Verilator model is a
  library the testbench advances with `eval()`. There is no VPI, and the simulator is not
  the master (the opposite of cocotb). DPI-C is allowed for in-cycle behavioural models.
- **Single source of truth.** Testbench types (structs, unions, enums, params, ports,
  interfaces) are generated from the elaborated RTL, never written by hand.
- **Tests read like SystemVerilog,** in the style of cocotb 2.x: async/await triggers,
  fork/join, events, queues.
- **Cargo-native** (`cargo test`/nextest), deterministic, seeded.

## Inputs to read
1. `../PLAN.md`: the plan, revised after adversarial reviews.
2. `../simjig/CLAUDE.md`: the constitution.
3. `../simjig/ARCHITECTURE.md`: the current skeleton.
4. `~/.claude/skills/agentic-rust/SKILL.md`: Rust rules.

The workspace currently has 5 empty crates (`svtypes`, `simjig-bits`, `simjig-build`, `simjig`,
`simjig-macros`) plus `xtask`. You may propose a different structure.

## Verified facts the architecture must respect
These come from Verilator master, 5.053-devel.

- **`--json-only` output:**
  - It is produced after parameter specialisation.
  - It has no `width` field: widths must be computed. Member `lsb` is a string and is
    omitted when it is 0. Values are Verilog literals.
  - The format changes between releases.
  - It turns `--timing` on by default, so pass `--no-timing` explicitly.
  - A DUT with interface ports on its top level can't be elaborated.
- **Model access:**
  - Ports are members of the model object.
  - Internal signals are reachable only if marked `public_flat_rd/rw` in a `.vlt` file.
    Writing non-public internals is undefined behaviour.
  - Public signals cost performance.
  - Upper bits must be masked on writes.
- **Fatal errors:** `$fatal`, `$stop` and internal fatals call `std::exit(1)` unless the
  `VL_USER_FATAL`/`VL_USER_STOP_MAYBE` hooks or `fatalOnError(false)` are used.
- **Eval and time:**
  - Edges are detected by comparing against the value at the last eval.
  - For several clocks, run one loop per timestamp: apply edges, eval, wake testbench
    tasks, let them write, eval, repeat up to a delta limit; then ReadOnly; then advance.
  - `--timing` DUTs expose `eventsPending()`/`nextTimeSlot()`.
- **Derived clocks:** pulses on internal derived or gated clocks inside one `eval()` are
  invisible to the testbench.
- **`--savable`:** it is incompatible with `--coverage` and `--timing`, and it can't save
  testbench futures. Use `fork()` copy-on-write checkpoints with `--threads 1` instead.
- **Multiple models in one process:**
  - Each needs its own `--prefix`.
  - DPI symbols are global.
  - The `$urandom` RNG is thread-local per context.
  - `--hierarchical` blocks are opaque.
- **Coverage:** counters are a flat `__Vcoverage[N]` array, usable as fuzzing feedback via
  the shim.
- **4-state:** `--fourstate` is experimental (it emits a FUTURE warning). Its encoding is a
  value plane plus an X/Z plane (same as VPI aval/bval). It is not merged yet.
- **Prior art:** Marlin (Rust, Verilator as library, sync, MPL-2.0), Rivet (Rust, VPI,
  cocotb-like), Hardcaml (cycle-based testbench, proven in production), and cocotbext-axi
  (about 5.6k LOC of bus models, MIT).

## Scope
### Now (M0–M2)
- Type IR and codegen.
- Typed DUT and signal handles.
- Scheduler with triggers, tasks, cancellation and a settle loop.
- Error and fatal routing.
- ValidReady, AXI-Stream and AXI4-Lite.
- Randomize derive and sequences as data.
- Scoreboards, end-of-test checks, coverage.
- Multi-clock, `--timing` DUTs.
- Shrinking, lockstep, fuzzing, `fork()` checkpoints.
- A cocotb Python back end for the type IR.

### Future
- 4-state values.
- AXI4 full and more VIP.
- A register model generated from SystemRDL.
- A separate project for properties and assertions, built on simjig's hooks.
- Large SoCs with hierarchical builds.

### Non-goals
- Other simulators, VHDL, a UVM port, encrypted vendor IP.

## Quality bar
The result must be easy to maintain, have a solid structure, and be easy to debug and
support:
- clear boundaries and a dependency direction;
- one place per concept;
- deterministic behaviour;
- actionable errors;
- observability (logs, traces, waveforms, repro commands);
- testable at every layer;
- friendly to AI agents (small crates, stable APIs, fast feedback).

## Deliverable
Write your proposal to the file named in your instructions. Keep it under about 2500 words
and use these sections:
1. Guiding principles (your top 5, and what you optimise for).
2. Crate and module layout, with the dependency graph and the reason for each boundary.
3. Data flow: SV sources → IR → generated code → build → runtime → reports.
4. Core runtime design: scheduler loop, trigger/waker model, tasks and cancellation,
   signal-handle ownership and safety, time model, multi-clock, `--timing`.
5. FFI/shim boundary: what is generated, what is hand-written, and the C ABI surface.
6. Codegen pipeline and the type IR schema sketch, with versioning against Verilator
   changes.
7. Error model, debuggability and observability (failure → triage → repro → waves).
8. Testing strategy for the framework itself.
9. Extension points for the future scope (how each lands without redesign).
10. Key Rust type/trait sketches (short code).
11. Top risks and what you would deliberately NOT build.
12. Mapping to milestones M0–M2.

Be concrete and opinionated. Don't hedge between options; pick one and justify it.
