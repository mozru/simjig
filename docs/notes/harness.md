# Harness notes

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
which model it needs. The generated `Dut` implements both (through `dut_model!`).

## Attribute grammar
```text
#[simjig::test]
#[simjig::test(timeout = "<integer><unit>" | "none")]  unit: fs ps ns us ms s; "1_000 ns" ok
#[simjig::test(crate = <path>)]                      simjig under another name
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
`<root>/<package>/<binary>/<test with :: -> __>/<seed, 16 hex>/`, replaced on every run
(pass or fail). `<root>` is `SIMJIG_RUNS_DIR`, else `<target>/simjig/runs`, where
`<target>` is the nearest ancestor of the test binary (within five levels) holding
`CACHEDIR.TAG`. `<package>` and `<binary>` (the test binary as nextest's `binary()`
filter names it) keep same-named tests of two test files or packages apart. A run is
written into a temporary sibling (`.<seed>.tmp-<pid>-<n>`) and renamed into place, so
concurrent runs of one test and seed each leave a complete directory (last writer wins).
See `crates/simjig/src/harness/rundir.rs`.

`summary.json`:
```text
schema "simjig.run/1" | test (libtest name) | package | launcher (e.g. "pixi run")|null
status "pass"|"fail" | seed "0x%016x" | seed_source "env"|"test-name" | verilator_seed u32
init "zero"|"random" | init_seed u32|null (model 0's)
precision "1ns"|null | end_time {ticks, display}|null
timeout {ticks, display, source "default"|"attribute"}|null
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
One block per failure (`FailureReport`'s `Display`): header `FAIL <test> seed= [init=random]
t= [Kind sig=]`, then `task`, `at`, `message`, kind-specific lines (`blocked:` uses
`BlockedTask`'s `Display`, as `Failure`'s does), `help`, `repro`, `waves` (docs/notes/waves.md),
`run dir`. Times print in the largest unit that shows them exactly (`TimeDisplay`:
`1250 ns`, `1 us`). The repro line is
`SIMJIG_SEED=<seed> [<launcher> ]cargo nextest run -p <package> [--run-ignored all] -E
'binary(=<binary>) & test(=<test>)'`, the launcher (`pixi run`, `conda run -p ..`) when
the test ran inside one. Signature = FNV-1a (`kernel::recorder::Fnv`) of kind, location
and the message with numbers masked (words starting with a digit, sized SystemVerilog
literals such as `8'h08`, which is how `Bits` values print, or all-hex words containing a
digit). Not included: time, seed, task path (they vary per seed).
`run_test` prints the block once to stderr and fails with `resume_unwind` (no extra
"panicked at harness" line); the payload is the block, so
`#[should_panic(expected = ...)]` works.

## Deviations from the original design
- **`sim: &Sim` is impossible with the current kernel.** `Sim::run(&mut self, main: impl
  Future + 'static)` borrows the `Sim` mutably for the whole run and needs a `'static`
  main future, so the body cannot hold `&Sim`. The context parameter is
  `&simjig::TestCtx` instead (test name, `seed()`, `rng(path)`, `verilator_seed()`); the
  running sim is reached through the free functions (`now()`, `spawn`). Writing `&Sim`
  gives a rustc error at the parameter type ("expected `&Sim`, found `&TestCtx`").
  Getting `&Sim` would need a kernel change (e.g. `Sim::run(&self)` with clocks behind a
  `RefCell` and a non-`'static` main), which needs `unsafe` or a different task store.
- **TaskPanic locations** come from the kernel (`kernel::panics` captures the location of
  the exact poll or drop that panicked). The harness installs no panic hook.
- **`help:` text** is taken from the last line of the kernel's `Failure` `Display`
  (`FailureKind::help` is private to the kernel). Covered by the per-kind snapshots.
- **Default timeout**: without `timeout`, a test stops at 10 ms of simulation time
  (`DEFAULT_TIMEOUT_MS`), `ceil(10 ms / tick)` ticks at precisions coarser than 1 ms;
  `timeout = "none"` disables it.
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
- Concurrent runs of one test and seed: covered by
  `concurrency::concurrent_runs_of_one_test_all_write_their_run_dir` on one machine only.

## Known limitations
- A task dropped before it ever ran is warned about and recorded
  (`TraceKind::DroppedUnpolled`), but `summary.json` has no count of them: the recorder
  is a 4096-event ring, so the harness could count only the ones still in the window. An
  exact count needs a kernel counter.
- `Rng` has no `Bits<N>` draw (`Bits::new(u128::from(rng.below(1 << 16)))` per field),
  and `Bits<N>` has no exact narrowing getter (`to_u128` only; a `u32` needs
  `u32::try_from`). Generated packed structs have `to_bits()` but no `LowerHex`.
- Design parameters are generated as `i32` constants (SV `int`), so a size needs
  `usize::try_from(Dut::DEPTH)`.
- `ns()`/`ps()` convert to ticks of the running simulation's precision, so they are not
  `const fn` and panic outside a run: a table of clock settings must be a function.
- `cargo simjig help help` and `cargo simjig help a b` report "no command `help`".
