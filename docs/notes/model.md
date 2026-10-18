# Model notes (agent "model", branch `w2/model`)

Code: `crates/simjig/src/model/{raw,verilator,meta,bind,values}.rs` (plus an accessor added
to `abi.rs`), `crates/simjig/src/signal.rs`. Tests: `crates/simjig/tests/model_tests/`
(one binary, real Verilator models) and unit tests in `model/` (a pure-Rust fake vtable).
Checked against Verilator 5.052 (pixi), osx-arm64.

## Layers
| Module | What |
|---|---|
| `model::raw` | `RawModel`: owns one handle; every call through the vtable; validates the signal table once (id, width, storage, words, non-null pointer, readable) so reads and writes are safe; `Drop` destroys. One of the two `unsafe` modules of `model`. |
| `model::verilator` | `VerilatorModel`: the kernel's `ModelBackend`. Load checks, message draining, poisoning, final blocks in `Drop`. |
| `model::meta` | `SignalMeta`, `Storage`, `Access` (the codegen contract), `ModelMeta`, `Vtable`, `LoadError`, `link_model!`. |
| `model::bind` | `Binder` trait (codegen contract), `ModelBinder`, `instantiate`/`create` (the `DutModel` methods), `dut_model!`. |
| `model::values` | `pack_field`/`unpack_field` (word-level, proptested against a bitwise reference), `InvalidEnum`, `FieldDiff`. |
| `signal` | `In`/`Out`/`InOut`/`PublicRd`/`PublicRw<T>`, `RawWords`. |

## Behaviour
- **Load** (`instantiate`): vtable prefix equals the generated one; `ModelVtable::check`
  (ABI version, table size, IR hash); `create` with `ModelOptions::verilator_args()` as
  `commandArgs` (so `+verilator+seed+N` reaches the context); then the C++ table against
  `Dut::SIGNALS` slot by slot: slot, name, width, storage (unpacked storage is rejected:
  the ABI has no unpacked rows) and direction/access bits. Any mismatch is a
  `LoadError`, turned into a `ModelFatal` `Failure` at t=0 for the harness. A model whose
  construction reported an error is never evaluated and runs no final blocks.
- **Messages** are drained after create, every eval and `final`. DISPLAY text is printed
  with `print!` (so libtest/nextest capture it with the test; checked by
  `messages::display_reaches_the_test_output`, which runs a child test with and without
  `--nocapture`), WARNING as `%Warning: file:line: msg`, DROPPED as a count.
  FATAL/ERROR/STOP/EXCEPTION become a `ModelError`; the first one in an eval wins.
- **Failure text**: `$fatal in TOP.fatal at <file>:31: fatal: kind=0 $fatal`. The hook
  receives no scope for `$fatal`/`$error` (abi notes), so `describe` splits Verilator
  5.052's printed text `[t] %Fatal: f:l: Assertion failed in <scope>: <msg>`; any other
  shape is reported verbatim with the record's own `hier`. The time is the `Failure`'s.
  `$stop` reads `$stop at <file>:32: Verilog $stop`.
- **Status mapping**: OK -> Running; FINISH -> Finished (kernel: `DesignFinished`); STOP,
  or any error record -> error; FATAL, EXCEPTION, POISONED or an unknown status -> error
  **and poison**. A poisoned model never reaches C++ again (`eval` returns
  `the model can no longer be evaluated: <first error>`, `events_pending` is false, `Drop`
  skips `final` and only destroys). Tested on the fake vtable (eval count stays 1).
- **Writes** go through `RawModel::write`: masked to the width by `abi`, and a slot without
  `ACCESS_WRITE` panics naming the signal. Typed handles make that unreachable; a raw
  `RawSig::write_u64` on an output panics out of `Sim::run` (the panic happens while the
  kernel applies the write log, outside any task, so it is not a `Failure`).
- `--timing`: `events_pending`/`next_time_slot` pass through; `timing_dut` runs with no
  kernel clock (`handles::model_events_advance_time`).

## Typed handles
- `Copy`, `!Send` (via `RawSig`), `Debug` names the signal. `raw()` for kernel APIs
  (`Clock::new`, `cycles`). `name()`, `changed()` everywhere; `rising()`/`falling()` have
  an inline-const assertion `T::WIDTH == 1` (a post-monomorphisation error: `cargo build`
  reports it, `cargo check` does not). Writing an `Out` or `PublicRd` is a `compile_fail`
  doctest (paired with a passing `get` doctest so the failure is the missing `set`).
- `get`/`set` need `T: Packed<Raw: RawWords>`, because `Packed::Raw` is only
  `Copy + Eq + Debug`. **Proposal for the bits owner:** bound `type Raw` by a storage trait
  in `simjig-bits`; then every bound is just `T: Packed`.
- `get` does one kernel lookup and asserts the slot width equals `T::WIDTH` (a handle typed
  for another width would silently read the wrong bits; codegen guarantees it, the check
  costs one compare). `set` checks it under `debug_assertions` only (it would cost a second
  lookup).
- **Out-of-range literals name the signal.** `set(impl Into<T> + 'static)` keeps
  DECISION §6's `Into`; before converting, it tests (by `TypeId`, folded away per
  monomorphisation) whether the argument is a `u128`, i.e. an integer literal, and checks
  it against `T::WIDTH`: `signal `wide.i8` is 8 bits wide: value 0x15a does not fit
  (maximum 0xff)`, at the caller's line. The conversion runs inside `set`, so the bits
  agent's concern (the conversion panics before `set` can name the signal) does not apply.
  Cost: a `'static` bound on the argument, which every value type meets.
- Handles are generic over any `T: 'static` (codegen names `In<[T; N]>` for unpacked
  arrays); those have no `get`/`set` until the ABI describes unpacked storage.

## Generated code: what codegen emits and what is missing
`model_tests::codegen` compiles all 14 files in
`crates/simjig-build/tests/codegen_it/generated/` against the real `__private` (not the
stub): they build unchanged, and struct packing, enum `variant()` and `__bind` work.
Two gaps, for the director (I did not edit codegen):
1. **Nobody emits the model glue.** Codegen emits `Dut { fields, IR_HASH, SIGNALS, __bind }`
   but nothing names the vtable symbol, the prefix or the time precision, and no one
   implements `DutModel`. Proposed: the generated file also contains one line
   ```rust
   ::simjig::__private::dut_model!(Dut, prefix = "Vcounter",
       symbol = "simjig_vtable_Vcounter", precision_exponent = -12);
   ```
   which expands to `link_model!`, a `static ModelMeta` and `impl DutModel for Dut`
   (`model::bind` docs). Codegen knows all three values (`ShimOptions.prefix`, the IR's
   `TimeExp`). The `unsafe extern` block lives inside the macro, so it compiles under a
   user's `#![forbid(unsafe_code)]` (checked: `unsafe_code` does not fire on code expanded
   from another crate's `macro_rules!`; a plain generated `unsafe extern` block would).
2. **`Dut` should `#[derive(Clone, Copy, Debug)]`.** Every field is a `Copy` handle; without
   it a test cannot pass `dut` to a spawned task and keep using it. The model tests add the
   derive in their hand-written instances (`model_tests/duts.rs`, otherwise the codegen shape
   plus `dut_model!`).

## Conformance against real models
`model_tests::conformance` replays 9 goldens on the real `flop`, `counter` and `comb_chain`
models through typed handles, byte for byte against `tests/conformance/golden/` (read
directly, no copies): `edge_read`, `readonly_read`, `write_after_edge`, `timeout`, `timer`,
`clock_cycles`, `value_change`, `first_combine`, `comb_settle` (s1..s3 through
`PublicRd` with a `.vlt`). All pass unchanged; the kernel's FakeModel replicas were
faithful. Not replayed: `two_clocks`, `cdc_stream`, `fifo_stream`, `derived_edge`
(DUTs not in this brief).

## Spike findings (docs/notes/spikes.md)
1. `threadContextp` before every entry point: already done by `simjig_rt`'s `Scope`
   (`simjig_rt.cpp:55`), for create, eval, final and the rest. Not tested with interleaved
   models, because one `Sim` drives one model and runs cannot interleave.
2. **`threads(1)` is NOT set: measured 13 threads in a test process after loading one model**
   (`ps -M`), i.e. an 11-worker `VlThreadPool` per context. The fix is one line in
   `simjig::Runtime::Runtime` (`crates/simjig-build/cpp/simjig_rt.cpp`, not mine):
   `m_context->threads(1);` before the model is constructed. Needed before fork (Stage 3)
   and to stop wasting CPU under nextest.
3. `$urandom` per-model `VlRNG` swap: Stage 3 (needs runtime C++). Today each context
   creation reseeds the thread stream, so `$urandom` depends on how many models a process
   created before. The verilator seed plusarg is passed.
4. Initial eval before the first edge: holds by construction. The kernel's first eval at
   t=0 runs before any write (clock edges included) is applied.
   `handles::a_clock_edge_at_time_zero_is_captured` checks a clock rising at t=0 clocks the
   counter.
5. Poisoning after FATAL: done (above). DPI panics: Stage 3 (no DPI support yet).

## Measured (release, Apple M6, `cargo test --release -p simjig --test model_tests -- --ignored --nocapture measure`, 3 runs)
| Access (inside `Sim::run`) | ns |
|---|---|
| `Out<Bits<8>>::get` | 3.2 - 4.1 |
| `Out<WideBits<200, 7>>::get` | 4.8 - 5.0 |
| `In<Bit>::set` | 7.1 - 7.6 |
| `In<Bits<64>>::set` | 6.7 - 7.2 |
| `RawSig::write_words` (kernel only) | 7.1 - 7.4 |

Reads meet the 5 ns budget (DECISION §4 Q2). Writes do not, and the typed layer adds
nothing measurable: the cost is the kernel's write log (`Writer::current`, the conflict
check, `Vec` pushes). That is the kernel owner's to optimise if it matters; a write is
logged once per delta, not per eval.

## Why not simpler (constitution rule 5)
- `RawWords` with a sealed `Buffer` supertrait: generic code cannot size a stack buffer
  from `T::WIDTH` on stable, so the storage type carries its buffer type; no allocation on
  any access.
- The fake vtable (`raw::fake`, test-only) is the only way to test poisoning and teardown
  order: the kernel stops at the first failure, so a second eval is unobservable through
  `Sim`.
- `dut_model!`/`link_model!` are macros because the extern declaration must be in the user
  crate (the symbol is defined there) but must not trip `unsafe_code` there.

## Not verified / open
- Linux. ASan/UBSan on the real path. Miri: the fake-vtable tests use raw FFI shapes and
  were not run under Miri.
- `InOut` and `PublicRw` against a real model (no DUT in the list has them); covered only
  by type checks and the codegen compile test.
- `kernel::ModelError` is a string: file, line and scope are in the text, not fields.
  A structured `ModelFatal { file, line, scope, kind }` would need a kernel change.
- `Binder` cannot check a handle's width (`T: 'static` only, per the contract); `get` does.
- The test models are built by `crates/simjig/build.rs` (hand-written shim, `--timescale
  1ns/1ps` for the DUTs); they should move to `simjig-build` now that it exists.
- `create` in `model::bind` resolves every signal by name through `Sim::signal` (O(n^2) in
  the signal count, once per test) because `Sim` keeps its simulation id private.
