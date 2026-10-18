# simjig

Rust testbenches that drive Verilator models. The testbench owns the loop and simulation
time; the model is a library. Types and a typed `Dut` are generated from the
SystemVerilog, so ports, packed structs, enums and interface bundles are Rust types.

See `ARCHITECTURE.md` for the code map and `docs/architecture/DECISION.md` for the design.

## Quick start

The canonical starting point is [`examples/quickstart`](examples/quickstart): two tiny
designs, a build script and tests. Copy it to start a project.

```rust
// build.rs
//! Verilates the flop and links it into this crate's tests.
fn main() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("flop")
        .sources(["rtl/flop.sv"])
        .top("flop")
        .build()?;
    Ok(())
}
```

```rust
// tests/flop.rs
//! The flop captures `d` on the rising edge.
use simjig::{ReadOnly, clock, ns};
use simjig_bits::Bit;

simjig::include_dut!(flop); // the generated `flop::Dut`

#[simjig::test(timeout = "1us")]
async fn q_follows_d(dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    dut.d.set(1);
    dut.clk.rising().await;
    ReadOnly::new().await; // the values the edge produced
    assert_eq!(dut.q.get(), Bit::ONE);
}
```

From this repository's root:

```sh
pixi install --locked          # Verilator 5.052 and the C++ toolchain, in .pixi/
pixi run cargo xtask setup     # the pinned cargo tools: cargo-nextest, cargo-deny, ...
pixi run cargo install --locked --path crates/cargo-simjig   # the `cargo simjig` command
pixi run cargo simjig doctor   # checks Verilator, the compiler and nextest, with fixes
pixi run cargo nextest run -p quickstart
```

Each file opens with a `//!` line because the repository's lints (`[lints] workspace =
true`, as in the quickstart) warn on `missing_docs`, and a build script and each test
file are crates of their own.

`cargo xtask setup` and `cargo install` put their binaries in `~/.cargo/bin`, which must
be on `PATH`. Run cargo through `pixi run`, so that it finds Verilator. To use simjig
outside this repository, see the quickstart's README ("Using it outside this
repository").

A failing test prints a report with the seed, the simulation time, the failing task and a
one-line `repro:` command that reruns it with the same seed.

Every `#[simjig::test]` has a simulation-time budget: 10 ms unless it sets
`timeout = "..."` (or `timeout = "none"`). For hangs that do not advance simulation time,
the quickstart's `.config/nextest.toml` sets a wall-clock `slow-timeout` with
`terminate-after`.

## The generated module
`simjig::include_dut!(fifo)` includes the Rust module that the build script generated
for the DUT `fifo`, under a hashed directory of `target/`. To read it (the names of its
types, constants and fields), print it:

```sh
pixi run cargo simjig show fifo            # the module, after a `//` line with its path
pixi run cargo simjig show fifo --path     # only the path
pixi run cargo simjig show fifo -p mycrate # the DUT of one package of a workspace
```

`show` runs `cargo check --tests` first (which runs the build scripts), so the file is
current; run it in the crate whose `build.rs` builds the DUT, or name that crate with `-p`.
The module is printed shortened for reading (`In<Bit>` for
`::simjig::__private::In<::simjig::__private::bits::Bit>`); the file at the path is the
exact text.

### Generated names
Each SystemVerilog name becomes a Rust name by these rules (`codegen::names`,
`codegen::lower::types`):

| SystemVerilog | Rust |
|---|---|
| port or struct field `data_in`, `dataIn`, `DATA_IN` | field `data_in` (`snake_case`) |
| a name that is a Rust keyword: `in`, `type` | `r#in`, `r#type` (`dut.r#in.set(1)`); `self`, `super`, `crate` become `self_`, ... |
| a name starting with a digit: `2nd` | `_2nd` |
| typedef `status_t`, `op_e`, `item_s`, `word_u` | type `Status`, `Op`, `Item`, `Word`: one `_t`/`_e`/`_s`/`_u` suffix is dropped, then `CamelCase` |
| two typedefs that would get one name (`a::state_t`, `b::state_t`) | both keep the suffix (`StateT`), then add the scope (`AStateT`, `BStateT`), then a number |
| an anonymous struct or enum, first used by port `in_anon` | `InAnon`, named after its first use |
| enum `status_t` with `ST_EMPTY`, `ST_FULL` | `Status::EMPTY`, `Status::FULL`: a `WORD_` prefix that every variant shares is dropped (unless a remainder would start with a digit: `MODE_1` stays `MODE_1`) |

An enum type is a newtype over its bits, so it also holds values outside its variants:
`Status::EMPTY` is a constant, `s.variant()` returns `Ok(StatusVariant::Empty)` (a Rust
`enum` you can `match`, `CamelCase` variants) or `Err(InvalidEnum)`, and
`Status::from_bits`/`to_bits` convert. A packed struct is a Rust struct with public fields
(`Item { tag, data }`); a packed union has a getter and a constructor per view
(`w.bytes()`, `Word::from_bytes(..)`).

## Coming from cocotb

simjig's scheduler follows cocotb 2.1 on Verilator, checked against recorded cocotb runs
(`tests/conformance/README.md`, "Observed semantics"). The same test, translated:

| cocotb | simjig |
|---|---|
| `Clock(dut.clk, 10, "ns").start()` | `clock(dut.clk, ns(10)).start()` |
| `await RisingEdge(dut.clk)`, `FallingEdge`, `ValueChange` | `dut.clk.rising().await`, `.falling()`, `.changed()` |
| `await ClockCycles(dut.clk, 3)` | `simjig::cycles(dut.clk, 3).await` |
| `await Timer(10, "ns")` | `Timer::new(ns(10)).await` |
| `await ReadOnly()`, `ReadWrite()`, `NextTimeStep()` | `ReadOnly::new().await`, `ReadWrite::new()`, `NextTimeStep::new()` |
| `cocotb.start_soon(coro)` | `spawn("name", fut).detach()` |
| `task = cocotb.start_soon(coro)`; `await task` | `let task = spawn("name", fut);` `task.await` |
| `task.cancel()` | `task.cancel()` (or drop a `Task` that has run) |
| `await First(a, b)` | `first!(a, b)` (same output type; ties go to the leftmost) |
| `await Combine(a, b)` | `join!(a, b)` (a tuple of the outputs) |
| `await with_timeout(x, 10, "ns")`, `SimTimeoutError` | `with_timeout(ns(10), x).await` (`&mut task` for a Task that must keep running), `Err(Elapsed)` |
| `dut.x.value = 5` | `dut.x.set(5)` |
| `int(dut.x.value)`, `bool(dut.x.value)` | `dut.x.get().to_u128()`, `dut.bit.get().to_bool()` |
| `get_sim_time("ns")` | `simjig::now()`; print it with `now().display(ctx.precision())` (`1250 ns`) |
| `Event`, `Queue`, `Lock` | `simjig::{Event, Queue, Lock}` |

What to keep in mind:
- **Dropping a `Task` cancels it, and `let _ = spawn(..)` fails the test.** `spawn`
  returns a handle, and rustc's `must_use` help suggests `let _ = ...`; that drops the
  handle at once, so the task would never run and the test would pass without it. simjig
  fails the run instead (`Setup`, pointing at the `spawn` call). Call `.detach()` to run
  the task in the background (cocotb's `start_soon`; it runs until its parent finishes),
  `.await` the handle, or `.cancel()` it. Only a handle dropped before its task ever ran,
  and never awaited, fails: dropping one whose task has run cancels it, as does a
  combinator dropping a losing operand. Leaving a handle's scope early (`?`, `return`)
  drops it too: when that returns from the test's body (`main`, in the poll where it
  returns), the run ends with the body's result (its error, for `?`). Only there: the
  same early exit inside a `scope` or spawned task fails as `Setup`, so `.cancel()` the
  handle first or spawn it after the checks that may return.
- **Writes are deposits.** `set` is applied after the next eval, so reading a signal right
  after setting it returns the old value, and a write right after `rising()` is not
  captured by that edge.
- **`rising()` of a testbench clock sees pre-edge values**; the flops' new values are
  visible from `ReadOnly`, where everything has settled. A write in `ReadOnly` fails the
  run (`WriteInReadOnly`; cocotb raises `RuntimeError`). Edges of design-driven signals
  see post-eval values.
- **A `Timer` due at an edge resumes first**, before the edge is applied: it sees the
  pre-edge state, and wins a tie against the edge in `first!` and `with_timeout` (a
  timeout due exactly at the edge times out).
- **`set` takes** `impl Into<T>`: an integer literal (as `u128`) for any `Bit`/`Bits<N>`/
  `WideBits` signal, a `bool` for a `Bit`, a value of the signal's own type, or a generated
  enum's constant or packed struct. A literal that does not fit panics, naming the signal
  and its width. A typed integer converts with `u128::from`: `sig.set(u128::from(byte))`
  for a `byte: u8`. Comparisons work the same way: `x.get() == 0x12` and `0x12 == x.get()`
  infer the literal; a typed integer is compared as `x.get() == u128::from(byte)`. In an
  `assert_eq!`, prefer `assert_eq!(x.get(), Bits::new(want))` over
  `assert_eq!(x.get(), want)`: the width is inferred from the left, and a mismatch prints
  both sides alike (`32'h0a04a19f` vs `32'h9fa1040a`, not a hex left and a decimal right).
  (`From<u8>` for `Bits<N>`, or `PartialEq<Bits<N>>` for `u8`, would make
  `set(1)` and `assert_eq!(n, xs.iter().sum())` fail to infer, so neither exists.)
- **Every test has a 10 ms simulation-time budget** (`#[simjig::test(timeout = "...")]`
  changes it, `"none"` removes it); cocotb has none. A timestep that never settles fails
  instead of hanging: more than `DELTA_MAX` (64) evals between `ReadWrite` rounds, or more
  than `READ_WRITE_MAX` (10 000) `ReadWrite` rounds that wake a task (cocotb loops forever).
- **A timeout is a value**, `Err(Elapsed)`, not an exception: handle it or `.unwrap()` it.
- **Known differences** (full list with tests: `tests/conformance/README.md`, "Known
  differences from cocotb"):
  - **A `Task` passed by value to `first!`/`join!`/`with_timeout` is cancelled** when the
    combinator drops it (timeout or losing); cocotb keeps it running. Pass `&mut task` to
    keep it running.
  - **Cancellation cleanup** (`Drop`) runs when an operand is cancelled, one scheduling hop
    before cocotb's `finally` block would.
  - **A `Lock` or `Queue` grant that lands on a cancelled `first!` loser is passed on** to
    the next waiter; cocotb keeps the lock held or loses the item.
  - **A zero-tick `Timer`** fails when it is awaited, not when it is built.

## Beyond one clocked DUT

### Build options
Each `simjig_build::Dut` takes what its tests need:

```rust
simjig_build::Dut::new("core")
    .sources(["rtl/core.sv"])
    .top("core")
    .trace_fst(true)   // FST waves (TraceOptions, waves on failure)
    .timing(true)      // --timing: `#` delays and `@` in the design run in simulation time
    .coverage(true)    // --coverage: Sim::coverage, write_coverage, zero_coverage
    .build()?;
```

A DUT the crate can do without (one that needs a tool not every machine has, say) is
optional: handle its error instead of returning it, and gate its uses on the `cfg` that
`Dut::build` sets only when the DUT built, `simjig_dut_<name>` (declared either way, so
it raises no `unexpected_cfgs` warning):

```rust
// build.rs
if let Err(e) = simjig_build::Dut::new("fast_model").sources(["rtl/big.sv"]).top("big").build() {
    println!("cargo::warning=fast_model skipped: {}", e.to_string().lines().next().unwrap_or(""));
}
```

```rust
// tests/fast.rs
#[cfg(simjig_dut_fast_model)]
simjig::include_dut!(fast_model);
#[cfg(simjig_dut_fast_model)]
#[simjig::test]
async fn big_works(dut: fast_model::Dut) { /* ... */ }
```

An ungated `include_dut!` of a DUT that failed is a compile error naming the DUT and
its error: a failed build replaces its generated module with a `compile_error!` stub,
so a module left by an earlier build cannot compile against a model that is not linked.
A failed DUT's build script reruns when an input its configuration names changes: the
sources, include directories, `-f`/`-F` files and the sources, `-v` files, `-y` and
`+incdir+`/`-I` directories in them or in `verilator_args`. Not tracked: a positional
source without a `.v`/`.sv`/`.vh`/`.svh`/`.vlt` (or C++) suffix, and `$VAR` in paths.

If any source declares a `timeunit` (or `` `timescale ``), every module and package
must: Verilator stops on a mix with `%Warning-TIMESCALEMOD` (IEEE 1800-2023 §3.14.2.3).
Verilator 5.052 does not check interfaces. Add the `timeunit`, or give the rest a default with
`.verilator_args(["--timescale", "1ns/1ps"])`. The wrapper simjig generates for a DUT
with interface ports is exempt: it takes the design's unit.

### Waves on failure
A run is never traced while it first runs. When waves are asked for, the harness replays
the test with the same seed in a child process, traces the last part of the run, checks
that the replay ended the same way, and writes `waves.fst` and a GTKWave `waves.gtkw`
(marker at the failure) into the run directory. Each model of the test and each `fork`
branch gets its own file (`waves.model-1.fst`, `waves.branch-0.fst`), listed in the report.

```sh
SIMJIG_WAVES_ON_FAIL=1 pixi run cargo nextest run          # every failure gets waves (CI)
SIMJIG_WAVES=1 SIMJIG_SEED=0x... pixi run cargo nextest run -E 'test(=m::t)'   # one run
SIMJIG_WAVES=window:2us..3us ...                            # a chosen span
pixi run cargo simjig waves m::t --seed 0x... [-p <package>] [--window 1.5us..]   # one command
```

The report's `waves:` line is the command to run (or, after a replay, the files). The
model must be built with `.trace_fst(true)`; otherwise the line says so, and
`SIMJIG_WAVES_ON_FAIL` does not replay the failure (`SIMJIG_WAVES` still does, as a
determinism check).

`cargo simjig` is the `cargo-simjig` binary, installed once (from this repository's root,
or with `--path` pointing at it from your own crate):

```sh
pixi run cargo install --locked --path crates/cargo-simjig   # puts cargo-simjig in ~/.cargo/bin
```

`cargo simjig waves <test>` runs `cargo nextest run` in the current directory, so run it
in your crate or workspace; `-p <package>` picks the package in a workspace, and
arguments after `--` go to nextest.

### DPI-C imports in Rust
For `import "DPI-C" function int dpi_add(input int x, input int y)`, the generated module
has a `Dpi` trait with one method per import. Register an implementation on the DUT
before the first eval:

```rust
struct Adder;

impl dpi::Dpi for Adder {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x + y
    }
}

#[simjig::test]
async fn sums(dut: dpi::Dut) {
    dut.set_dpi(Adder); // first thing in the test, before any await
    dut.a.set(2);
    dut.b.set(3);
    Timer::new(ns(1)).await;
    assert_eq!(dut.sum.get().to_u128(), 5);
}
```

An implementation runs inside the model's eval: it cannot read or write signals, await,
spawn, or drive another simjig model (another DUT's signals or another `Sim`). Record what
it needs (an `Rc<RefCell<..>>` it shares with the test) and act on it in a task after the
eval. A panic fails the run as `DpiPanic`; a missing `set_dpi` fails as `Setup`, saying
what to do. Two instances of one DPI build cannot run in one simulation: build the design
a second time under another `Dut::new` name.

### Several DUTs, lockstep, fork
A test may take two to four DUTs; they share one simulation (one time axis, one delta
loop). `Mirror` drives an input of two models at once and `Lockstep` compares outputs per
cycle, reporting the first divergent cycle:

```rust
#[simjig::test]
async fn rev2_matches_rev1(old: counter::Dut, new: counter_rev2::Dut) {
    clock(old.clk, ns(10)).start();
    clock(new.clk, ns(10)).start();
    Mirror::new(old.en, new.en).set(1);
    let mut lockstep = Lockstep::new().output(old.count, new.count);
    for _ in 0..100 {
        old.clk.rising().await;
        ReadOnly::new().await;
        lockstep.step(); // panics with the divergence
    }
}
```

`simjig::fork::run_from_checkpoint` runs several branches of a test from one state (after
a long reset, say), each in a forked copy of the process with its own seed (Unix, under
nextest). The test takes a `&simjig::TestCtx` for the root seed:

```rust
#[simjig::test]
async fn after_reset(ctx: &simjig::TestCtx, dut: counter::Dut) {
    reset(dut).await; // the checkpoint
    let branches = fork::run_from_checkpoint(ctx.seed(), 8, |branch| async move {
        let mut rng = branch.rng("stim");
        drive(dut, &mut rng).await;
        Ok(dut.count.get())
    })
    .await
    .expect("a quiet point under nextest");
    let counts = branches.into_values(); // fails the test, naming every failed branch
}
```

Checkpoints do not nest: inside a branch, `run_from_checkpoint` returns
`ForkError::Nested`. Branches run one at a time; see the `simjig::fork` docs for the
other limits.

### Coverage
A model built with `.coverage(true)` exposes its counters: `Sim::coverage(model)` (or
`TestCtx::coverage` inside a test) reads them, `zero_coverage` clears them, and
`write_coverage(model, "coverage.dat")` writes Verilator's file with every point's name,
for `verilator_coverage`. `model` is the model index: 0 for the first DUT, then the
test's DUT parameters in order.
