# simjig architecture: consolidated decision (v1)

> **Scope update:** simjig is the Verilator-Rust interface layer only. Everything about
> verification methodology is out of scope: `simjig-vip`, scoreboards, choice-stream
> randomization, shrinking, fuzzing harnesses, covergroups and RAL. Below, those items
> are kept only as the rationale archive. simjig still *exposes* what such layers need:
> coverage counters, multiple models per sim, checkpoints and hooks.

Sources: `proposal-A.md` (systems, TigerStyle), `proposal-B.md` (Rust library) and
`proposal-C.md` (verification tooling), written independently from `BRIEF.md`. This file
is the decision; the proposals remain as the rationale archive.

## 1. Where all three agreed (adopted as-is)
- **Signal handles are `Copy` ids** (model, generation, slot), with the direction in the
  type. They are resolved through the running kernel. No Rust reference to model memory
  ever exists, and a stale handle panics, naming the signal.
- **One cancellation mechanism: drop.** Cleanup is synchronous `Drop`; a graceful stop is a
  cooperative stop token. There is no soft-cancel protocol.
- **Structured tasks.** Each poll runs under `catch_unwind`, and a panic becomes a
  `TaskPanic` failure carrying the hierarchical task name.
- **Normative scheduler loop, once per distinct timestamp:**
  1. apply clock edges due at t;
  2. eval;
  3. wake waiters on changed watched signals;
  4. run tasks;
  5. repeat until quiet, up to a delta limit;
  6. ReadOnly, where any write is a failure;
  7. advance to `min(wheel, nextTimeSlot)`.

  Clocks are kernel data, not tasks. Coincident edges share one eval. Triggers on
  internal derived clocks are documented as sampled.
- **One front end: Verilator `--json-only --no-timing`.** It is decoded in exactly one place
  per Verilator dialect, with widths computed (never trusted), strict about unknown nodes,
  and backed by a fixture corpus, insta IR snapshots and a nightly canary against
  Verilator master. **slang is dropped** from M0–M2; it can return later as a second IR
  producer.
- **Layout check:** a generated SV module asserts `$bits` and member offsets, and the shim
  `static_assert`s member sizes.
- **C ABI:** one generated `simjig_vtable_<prefix>()` per model, carrying `abi_version`,
  `ir_hash` and the Verilator version, all checked when the model loads. A hand-written C++
  runtime of about 300 lines provides the `VL_USER_*` hooks and the exception barrier.
- **Fatal routing:** record `{file, line, hier, msg, time}` and throw `FatalUnwind`. The
  eval wrapper catches it, and the model is poisoned (no further eval). M0 must prove that
  unwinding through generated code works. The fallback is record, `_exit`, and let the
  harness report.
- **Failures are data** (`Failure { kind, time, component, task, location, signature }`),
  with an actionable `Display`, a one-line repro, and waves from a deterministic replay.
  The replay doubles as the determinism check. Retries are never used.
- **The framework's own tests** use a pure-Rust fake model behind the same ABI (A) or
  backend (B, C), under Miri, plus swarm/random kernel programs, the cocotb conformance
  suite (~30 DUTs), ASan/UBSan on the shim, and snapshots of every failure rendering.
- **`fork()` checkpoints** (Unix, `--threads 1`).
- **Not built:** `--savable`, VPI, a solver, a `.toml` mapping language, a UVM
  factory/config_db, a multi-threaded executor, a wave viewer, retries.

## 2. Decisions where they disagreed
| # | Topic | A | B | C | **Decision** | Why |
|---|---|---|---|---|---|---|
| 1 | Crate count | 6, one deep runtime | 14 | 12 | **6 now** (A), with **named split points** | Constitution rule 6: flat and deep. Each split happens only when its trigger fires (§3). |
| 2 | Testbench writes | Immediate `ptr::write` | Write log applied before eval | Buffered deposit | **Write log (B/C)** | Matches cocotb 2.x deposit semantics and gives deterministic conflict detection (`WriteConflict` names both tasks). `Drop` cleanup still works, and every stimulus bit is traceable. |
| 3 | Randomness, shrinking, fuzzing | One recorded *choice stream* feeds all three | proptest strategies | proptest strategies | **Choice stream (A)** | One mechanism instead of three. libFuzzer bytes and Hypothesis-style shrinking work on the same stream. Named sub-streams derived from `hash(root_seed, component_path)` (B's SeedTree) keep the streams independent. proptest stays a dev-dependency for testing `simjig-bits`. |
| 4 | Logging | Fixed-size flight recorder, no `tracing` | `tracing` + `events.jsonl` | `tracing` + `events.jsonl` | **Flight recorder is the core** (A). It is dumped to `events.jsonl` (B/C format), and its trace hash is the determinism check. `simjig::log!` writes into it. A `tracing` bridge is optional and feature-gated (M1+) | Deterministic and allocation-free. One event model serves logs, triage and replay. |
| 5 | Codegen writer | Plain indenting writer | — | `quote` + `prettyplease` | **Plain writer for every emitter** (A), fed by **one lowering** (B) | C++, `.vlt` and SV need a text writer anyway. One mechanism gives byte-stable output and fast build scripts. |
| 6 | Cross-checks | SV layout module | + header `VL_IN/OUT(msb,lsb)` check on every build | Layout module | **Both** | The header check is cheap and catches drift on every build, not just in CI. |
| 7 | Enums | Typed enum; invalid value fails the read | Newtype with consts plus fallible `variant()` | — | **B** | SV enums can legally hold non-member values. Reads stay honest, and the test decides what an invalid value means. |
| 8 | Time unit | Picoseconds; reject finer precision | Ticks of the design's `timeprecision` | Ticks | **Ticks (B/C)** | Supports fs-precision designs. Durations that can't be represented are errors, never rounded. |
| 9 | Interface-ported tops | Wrapper (parameters unspecified) | `build.rs` `.iface_port(..)` | `.bind_iface(..)`, and if missing, an error that prints the line to add | **C** | The testbench is the real instantiator. The error tells the user exactly what to write. |
| 10 | libverilated | Per-model `verilator --build`, cached | A `-sys` crate builds it once per workspace via `links` | Per model build | **Built once per (Verilator version, hook defines) in the shared cache**, by `simjig-build` | Gets B's build-time win without a sys crate. |
| 11 | Tooling CLI | — | `svtypes` CLI | `cargo simjig doctor/triage/waves/regress/new-vip` | **C, reduced to:** `doctor` (M0), `waves` (Stage 2); the methodology commands (`regress`, `new-vip`) are out of scope | Supportability is a stated goal. `doctor` makes setup reproducible. |
| 12 | `unsafe` location | 3 modules in `simjig` | `-sys` plus one module | One crate | **A:** `simjig::{model::raw, kernel::waker, fork}` only. Everything else `forbid(unsafe_code)`. | Same safety without an extra crate. |

## 3. Crate layout (v1)
```
build time                         run time
svtypes (serde_json)               simjig-bits
   ^                                  ^
simjig-build ───(generated code)──> simjig
                                      ^
                                   simjig-macros
```
| Crate | Owns |
|---|---|
| `svtypes` | IR schema (versioned), Verilator dialect front ends, width calculator, literal parser, Python/cocotb emitter |
| `simjig-bits` | `Bits<N>`, `WideBits<N, W>`, the `Packed` trait, `sv!`/`cat!`, explicit width changes |
| `simjig-build` | The lowering and all emitters (Rust, C++ vtable, `.vlt`, wrapper SV, layout check), Verilator invocation, header cross-check, content-hash cache, the hand-written C++ runtime sources |
| `simjig` | Kernel (`kernel/`: executor, wheel, phases, sync; no FFI imports), model FFI (`model/`), signals, failures and report, recorder, seeded RNG streams, coverage-counter access, multiple models per sim, fork |
| `simjig-macros` | `#[simjig::test]`, `#[derive(Bundle)]`, `#[derive(Arbitrary)]` |

**Split triggers** (from B/C, deferred until needed):
- `simjig::kernel` becomes `simjig-exec` when the properties project needs it.
- `simjig::report` becomes `simjig-report` when `cargo-simjig` must read run directories
  without linking the simulator.
- `simjig-verif` splits out when methodology code outgrows the runtime crate.

## 4. Open questions for M0 spikes
1. Does throwing a C++ exception through Verilator-generated code unwind cleanly,
   including `--timing` coroutines? If not, use the fallback.
2. The cost of looking up a signal through the thread-local kernel on every access. Budget:
   5 ns or less; if it's more, cache base pointers in `Dut` (the API stays the same).
3. Does `fork()` work inside a nextest test process (single thread, `--threads 1`, no open
   trace files)?
4. Verilator's JSON fields for interface/modport and parameter clones, on the pinned
   version (5.052, from conda-forge).

## 5. Corrections to earlier documents
- Marlin is **MPL-2.0**, not MIT (checked against its LICENSE file). Borrow ideas, not code.
- The `agentic-rust` skill's §9a ("drive on falling edge, 2 evals per cycle", "no
  `--timing`") is superseded by the per-timestamp loop and `--timing` support above.

## 6. API conventions (decided)
- **Rust-native signal access:** `sig.set(v)` and `sig.get()`. Plain assignment can't be
  overloaded in Rust, and operator tricks (`<<=`, implicit reads in `==`, macro blocks)
  are rejected because they hide reads and writes of the model.
- `set` takes `impl Into<T>`, so literals work (`dut.data.set(0x5a)`). A value that
  doesn't fit fails at the caller's line (`#[track_caller]`), naming the signal and its
  width.
- 1-bit signals use `Bit`, which converts from `bool` and from `0`/`1`.
