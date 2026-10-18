# Harness notes (agent "harness", branch `w2/harness`)

Code: `crates/simjig-macros/` (the attribute), `crates/simjig/src/harness.rs` and
`crates/simjig/src/harness/` (runtime). Tests: `crates/simjig/tests/harness_tests/` (one
binary), UI cases in `crates/simjig/tests/harness_ui/`, macro snapshots in
`crates/simjig-macros/src/snapshots/`.

## The `DutModel` seam
```rust
pub trait DutModel: Sized + 'static {
    fn instantiate(options: &ModelOptions) -> Result<Sim, Failure>; // build model + Sim
    fn create(sim: &mut Sim) -> Result<Self, Failure>;              // bind handles, add clocks
}
#[non_exhaustive] pub struct ModelOptions { pub seed: Seed, pub verilator_seed: u32 }
impl ModelOptions { pub fn verilator_args(&self) -> Vec<String> } // ["+verilator+seed+<n>"]
```
`()` implements it (an empty `FakeModel` at 1 ps) for tests without a DUT. The brief's
`create(sim: &mut Sim)` is kept as is; `instantiate` was added because a `Sim` is built
*from* a backend, so something must build the backend first, and only the DUT type knows
which model it needs. The generated `Dut` (simjig-build + the model agent) implements both.

## Attribute grammar
```text
#[simjig::test]
#[simjig::test(timeout = "<integer><unit>")]          unit: fs ps ns us ms s; "1_000 ns" ok
async fn <name>([<pat>: &<Ctx>,] [<pat>: <Dut>]) [-> <Output>] { ... }
```
- A `&T` parameter is the context, a by-value one the DUT; context first; at most one of
  each. Not `const`/`unsafe`/`extern`/generic/variadic/method. All of these are spanned
  `syn::Error`s, reported together (UI tests in `tests/harness_ui/`).
- Output: `()` or `simjig::Result` (`Result<(), Failure>`); anything else fails
  `TestOutput` at the user's return type.
- Expansion: a zero-argument `#[test]` with the user's attributes and visibility, holding
  the user's fn unchanged (minus attributes/visibility) and a call to
  `simjig::__private::run_test::<Dut, _, _>(&TestMeta { .. }, |ctx, dut| async move { .. })`.
  The inner fn carries `#[allow(clippy::unused_async, clippy::result_large_err)]`: whether
  they fire depends on user code, so `expect` would itself warn.
- `#[ignore]` is detected so the repro line adds `--run-ignored all`.

## Run directory schema
`<target>/simjig/runs/<test with :: -> __>/<seed, 16 hex>/`, replaced on every run (pass or
fail). `<target>` = nearest ancestor of the test binary holding `CACHEDIR.TAG`; override
with `SIMJIG_RUNS_DIR`.

`summary.json`:
```text
schema "simjig.run/1" | test (libtest name) | package | status "pass"|"fail"
seed "0x%016x" | seed_source "env"|"test-name" | verilator_seed u32
precision "1ns"|null | end_time {ticks, display}|null | timeout {ticks, display}|null
trace_hash "0x%016x"|null | events {file:"events.jsonl", written, recorded}|null
failure null | {kind, signature, time {ticks, display}, task|null, location|null,
                message, repro, waves, report (the printed block)}
```
`events.jsonl`: the recorder window (last 4096 events), one per line:
`{"t":ticks,"kind":"<TraceKind>","task":<index<<16|gen>|null,"arg":u64,"value":u64}`.
`null`s mean the model could not be built.

## Seeds
Root: `SIMJIG_SEED` (`0x` hex or decimal, `_` allowed; a bad value panics with the accepted
forms) else `mix64(fnv1a(package "::" test))`. Stream `path`: `xoshiro256**` seeded via
SplitMix64 from `mix64(root ^ mix64(fnv1a(path)))`. Verilator: stream `"verilator"`,
`& 0x7fff_ffff`, min 1 (Verilator stores an `int` and treats 0 as "random",
`verilated.cpp`). Known-answer tests pin xoshiro and SplitMix64 to the reference C.

## Failure report
One block per failure (`FailureReport`'s `Display`): header `FAIL <test> seed= t= [Kind
sig=]`, then `task`, `at`, `message`, kind-specific lines, `help`, `repro`, `waves`
(placeholder), `run dir`. Signature = FNV-1a of kind, location and the message with
numbers masked (words starting with a digit, or all-hex words containing a digit). Not
included: time, seed, task path (they vary per seed). `run_test` prints the block once to
stderr and fails with `resume_unwind` (no extra "panicked at harness" line); the payload is
the block, so `#[should_panic(expected = ...)]` works.

## Deviations and contradictions (report to the director)
- **`sim: &Sim` is impossible with the current kernel.** `Sim::run(&mut self, main: impl
  Future + 'static)` borrows the `Sim` mutably for the whole run and needs a `'static`
  main future, so the body cannot hold `&Sim`. The context parameter is
  `&simjig::TestCtx` instead (test name, `seed()`, `rng(path)`, `verilator_seed()`); the
  running sim is reached through the free functions (`now()`, `spawn`). Writing `&Sim`
  gives a rustc error at the parameter type ("expected `&Sim`, found `&TestCtx`").
  Getting `&Sim` would need a kernel change (e.g. `Sim::run(&self)` with clocks behind a
  `RefCell` and a non-`'static` main), which needs `unsafe` or a different task store.
- **TaskPanic locations**: the kernel does not capture them. The harness chains a process
  panic hook (installed once) that records `(payload, location)` per thread during a run
  and attaches the latest one whose payload equals the `TaskPanic` message.
- **`help:` text** is taken from the last line of the kernel's `Failure` `Display`
  (`FailureKind::help` is private to the kernel). Covered by the per-kind snapshots.
- **No timeout by default**: without `timeout`, a test with a free-running clock that never
  ends runs until nextest's slow-timeout. A default needs a project decision.
- `ARCHITECTURE.md` is not mine: its code map should gain "`simjig::harness`: seeds, test
  runner, failure reports, run directories" and `simjig-macros`' now real `#[simjig::test]`.
- `clippy::result_large_err` fires on every `-> simjig::Result` (Failure is 144 bytes).
  The macro silences it on the test fn; generated `DutModel` impls in trait position do not
  trigger it.

## Why not simpler (constitution rule 5)
- **Proc macro**: the input is a user function's syntax, and the errors must point at it.
- **Hand-written JSON writer (≈60 lines)** instead of `serde_json` in the runtime crate:
  output-only, four value kinds; `serde_json` stays a dev-dependency that parses it back.
- **In-crate PRNG** (≈40 lines): a printed seed must replay the same values forever.
- **`__private::run` + `RunEnv`**: `run_test` is `run` plus process state (env, panic);
  splitting them lets tests run the harness without `set_var` (unsafe in edition 2024).
- New dependencies: `syn` 3, `quote`, `proc-macro2` (macro); `prettyplease`, `insta`
  (macro snapshots); `trybuild`, `serde_json`, `insta` (simjig dev-deps).

## Verified
- `pixi run cargo xtask ci`: all steps pass (330 nextest tests, doc tests, deny, typos,
  machete).
- Mutation: making `RunEnv::from_process` ignore `SIMJIG_SEED` fails
  `repro::simjig_seed_reproduces_a_seeded_failure`.
- Type errors for a wrong context type, a non-`DutModel` DUT and a bad output type point
  at the user's tokens (checked by hand; trybuild cannot cover them, because a `#[test]`
  fn is not type-checked outside `--test` builds).

## Not verified
- No real Verilator `Dut`: the seam is exercised only with FakeModel DUTs.
- `include_dut!(name)` (the `OUT_DIR` form) is untested; only `@from <dir>` is, since no
  build script of mine writes `OUT_DIR/simjig/`.
- Waves: the `waves:` line is a placeholder; `SIMJIG_WAVES` is read by nothing.
- Run dirs under concurrent runs of the same test and seed (two processes) can race.
