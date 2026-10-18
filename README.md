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

A new crate needs a target before its first build: with only `Cargo.toml` and
`build.rs`, cargo stops with `no targets specified in the manifest`. Add the first test
file (`tests/<name>.rs`) or an empty `src/lib.rs`. A library target is fine, for
helpers that several test files share: the models link into the tests either way.

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
| a struct typedef `beat_t` inside an interface (the generated wrapper's port `up_beat` spells it out) | `Beat`, after the typedef, then `UpBeat` if that name is taken |
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
| `await First(a, b)` | `first!(a, b)` (same output type; ties go to the leftmost; to tell the winner, end each operand with a variant of your own enum and `match`) |
| `await Combine(a, b)` | `join!(a, b)` (a tuple of the outputs) |
| `await with_timeout(x, 10, "ns")`, `SimTimeoutError` | `with_timeout(ns(10), x).await` (`&mut task` for a Task that must keep running), `Err(Elapsed)` |
| `dut.x.value = 5` | `dut.x.set(5)` |
| `int(dut.x.value)`, `bool(dut.x.value)` | `dut.x.get().to_u128()`, `dut.bit.get().to_bool()` |
| `get_sim_time("ns")` | `simjig::now()`; print it with `simjig::now_display()` (`1250 ns`) |
| `dut._log.info("x=%d", x)` | `simjig::tb_log!("x={x}")` (prints `[1250 ns main.monitor] x=3`) |
| `Event`, `Queue`, `Lock` | `simjig::{Event, Queue, Lock}` |
| `random.random() < 0.5`, `random.randrange(n)` | `rng.bool()`, `rng.below(n)` on a seeded `ctx.rng("name")` |

A seeded stimulus stream and an in-order scoreboard, in a test taking `ctx: &TestCtx`
(the report's seed replays the stream):

```rust
let (mut rng, sb) = (ctx.rng("stim"), Queue::new("sb", 0)); // 0: unbounded
if rng.bool() { sb.put(rng.below(256)).await; }             // the driver records each beat
assert_eq!(dut.q.get().to_u128(), u128::from(sb.get().await)); // the monitor checks in order
```

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
- **A combinational design advances with a `Timer` per vector**: `set` the inputs, then
  `Timer::new(ns(1)).await` and `get` the outputs (the quickstart's `tests/inv.rs`).
  `ReadOnly` is read-only: a `set` after it in the same timestep fails the run
  (`WriteInReadOnly`), so the next vector needs the `Timer` anyway.
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

### Random initial values
Verilator is 2-state and starts every register at 0, so a flop that reset never clears
passes every test that resets at time 0. Ask for random initial values to catch it:

```rust
#[simjig::test(init = random)]   // or `init = zero`; without it, SIMJIG_INIT decides
async fn fills_and_drains(dut: fifo::Dut) { /* ... */ }
```

`SIMJIG_INIT=random cargo nextest run` turns it on for every test without an `init`
argument (the argument wins, so a test pinned to `init = zero` keeps zeros). Every variable
without an initializer, undriven inputs included, then starts from a value drawn from the
run's seed (`Seed::init_seed`), its scope and its name: the same seed gives the same start,
and a failure's report says `init=random` and its `repro:` line starts with
`SIMJIG_INIT=random SIMJIG_SEED=...`, which reproduces it exactly (`summary.json` records
`init` and `init_seed`). Only the start changes: `$urandom` draws as with zeros. In a
multi-DUT test, models of one design (one top module) start alike, so the two sides of a
`Lockstep` agree at cycle 0, and models of different designs start from their own streams;
fork branches inherit the start they were forked from. The model must keep Verilator's default `--x-initial unique`
(`.x_initial(XInitial::Unique)`); one built with `.x_initial(XInitial::Zero)` or
`XInitial::Fast` compiles its initial values in and refuses `init = random`. A raw
`--x-initial` among `verilator_args` is refused. `--x-assign` (explicit `X` in the source)
is a separate option and not affected.

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

Sources kept in a Verilator file list go in with `.file_list("rtl/files.f")` (`-f`:
paths relative to the package root) or `.file_list_relative("rtl/files.F")` (`-F`:
relative to the list). The lists are read in order, before `.sources([..])`, by every
Verilator run, so a package listed first is declared before the modules importing it; a
DUT may have a file list and no `.sources`. An `-f` among `verilator_args` is read after
the sources instead, which is too late for a package and does not count as sources.

A DUT the crate can do without (one that needs a tool not every machine has, say) is
optional: handle its error instead of returning it, and gate its uses on the `cfg` that
`Dut::build` sets only when the DUT built, `simjig_dut_<name>` (declared either way, so
it raises no `unexpected_cfgs` warning):

```rust
// build.rs
if simjig_build::Dut::new("fast_model").sources(["rtl/big.sv"]).top("big").build().is_err() {
    println!("cargo::warning=fast_model skipped");
}
```

A failed `Dut::build` has warned its cause already (`warning: <pkg>: simjig: ..`: the
headline, any hints and, under the default lint, the fatal warning), so the script's own
warning need only say what it skipped.

```rust
// tests/fast.rs
#[cfg(simjig_dut_fast_model)]
simjig::include_dut!(fast_model);
#[cfg(simjig_dut_fast_model)]
#[simjig::test]
async fn big_works(dut: fast_model::Dut) { /* ... */ }
```

The `cfg` reaches the targets of the package whose build script built the DUT (its
library, tests, examples, benches), not the crates that depend on it: a dependent that
needs the gate sets its own. A script that calls `Dut::build` only sometimes (behind a
feature or an environment variable) must still declare the gate when it does not call
it, or the gated code warns `unexpected_cfgs`:
`println!("cargo::rustc-check-cfg=cfg(simjig_dut_fast_model)");`.

An ungated `include_dut!` of a DUT that failed is a compile error naming the DUT and
its error: a failed build replaces its generated module with a `compile_error!` stub,
so a module left by an earlier build cannot compile against a model that is not linked.
A failed DUT's build script reruns when an input its configuration names changes: the
sources, include directories, `-f`/`-F` files and the sources, `-v` files, `-y` and
`+incdir+`/`-I` directories in them or in `verilator_args`. A directory is tracked by
the files directly in it (Verilator searches it one level deep; Cargo would scan it
whole, `target/` included, and rerun on every build). Not tracked: a positional source
without a `.v`/`.sv`/`.vh`/`.svh`/`.vlt` (or C++) suffix, `$VAR` in paths, a file added
to a directory later, and a subdirectory an `` `include "sub/x.svh" `` reaches.

A build script that returns the error fails the build, and Cargo prints, in order: the
script's warnings (simjig's `simjig: ..` cause first), then under `--- stdout` every
line the script printed, including those `rerun-if-changed` and `rerun-if-env-changed`
lines, and last under `--- stderr` the full error. simjig cannot leave the rerun lines
out. A script that handles the error needs them, or it would not rerun when the DUT is
fixed. And `Dut::build` returns before the script decides, so nothing is left to run
once the exit status is known. (A failed script reruns on every build anyway, so in
that case the lines are only noise.)

If any source declares a `timeunit` (or `` `timescale ``), every module and package
must: Verilator stops on a mix with `%Warning-TIMESCALEMOD` (IEEE 1800-2023 §3.14.2.3).
Verilator 5.052 does not check interfaces. Add the `timeunit`, or give the rest a default with
`.verilator_args(["--timescale", "1ns/1ps"])`. The wrapper simjig generates for a DUT
with interface ports is exempt: it takes the design's unit.

### Interface parameters
A Verilator top cannot have interface ports, so `.bind_iface("up", "bus_if.sink")`
makes the generated wrapper instantiate one `bus_if` per port. Its parameters follow one
rule:
1. **A value the binding sets always wins**:
   `.bind_iface_with_params("up", "bus_if.sink", [("WIDTH", 16)])`.
2. **Otherwise a `.param` of the same name is the default**: `.param("WIDTH", 16)` also
   gives each bound interface that declares `WIDTH`, and whose binding leaves it unset,
   `WIDTH` 16.
3. **Otherwise each side keeps its own default**, and when a parameter both declare ends
   up with two values (a module defaulting to 16, an interface to 8), the build warns
   (`cargo::warning`), naming both values and the `.bind_iface_with_params(..)` that
   settles it. Setting either side explicitly silences it; binding the interface's own
   value (`[("WIDTH", 8)]`) silences it without changing a width. Under Verilator's
   default lint such a pair usually stops the build at a `%Warning-WIDTH*` inside the
   generated wrapper instead; the error then starts with a `hint:` naming the two
   defaults.

The tie is by name only, a heuristic: an interface port header carries no parameter
assignments (IEEE 1800-2017 §25.5), so nothing in the design says that a module's
`WIDTH` and its port's `WIDTH` are one quantity. A module may read a wider interface
through its port (`out.W`), which rule 1 allows.

### A parameter sweep
To test several configurations of one module with one test body, build them as a
`Sweep` in `build.rs`. Each configuration is still its own DUT (its own model and
module), and the sweep adds one Rust module over them (`examples/lib`, `sync_fifo`):

```rust
let fifo = |name: &str, width: u32, depth: u32| {
    Dut::new(name)
        .sources(["rtl/sync_fifo.sv"])
        .top("sync_fifo")
        .param("WIDTH", width)
        .param("DEPTH", depth)
        .param("FWFT", 0)
        .trace_fst(true)
        .verilator_args(LINT)
};
Sweep::new("sync_fifo")
    .dut(fifo("fifo_8x4", 8, 4))
    .dut(fifo("fifo_32x16", 32, 16))
    .build()?;
```

`simjig::include_dut!(sync_fifo)` includes the sweep: each configuration's module
(`sync_fifo::fifo_8x4::Dut`), the generated types that are the same in every configuration
(defined once, so `fifo_8x4::Item` is `sync_fifo::Item`), and the trait
`sync_fifo::Ports`, which every configuration's `Dut` implements. Its methods return the
`Dut`'s fields (`dut.clk()` for `dut.clk`) and its associated consts are the parameters
(`D::DEPTH`). A test written once over `D: sync_fifo::Ports` runs on every configuration
listed in `duts`, one test each (`count_climbs_to_the_depth::fifo_8x4`), and a failure
points at the line in the body:

```rust
#[simjig::test(init = random, timeout = "2us", duts = [sync_fifo::fifo_8x4::Dut, sync_fifo::fifo_32x16::Dut])]
async fn count_climbs_to_the_depth<D: sync_fifo::Ports>(dut: D) {
    clock(dut.clk(), ns(10)).start();
    dut.rst().set(1);
    cycles(dut.clk(), 2).await;
    dut.clk().falling().await;
    dut.rst().set(0);
    dut.push().set(1);
    let depth = u128::from(D::DEPTH.unsigned_abs());
    for n in 1..=depth {
        dut.wr_data().set(n); // a `u128` that fits every configuration's `WIDTH`
        dut.clk().rising().await;
        ReadOnly::new().await;
        assert_eq!(dut.count().get(), n, "after {n} pushes, DEPTH {depth}");
        assert_eq!(dut.full().get(), n == depth);
        dut.clk().falling().await;
    }
}
```

A signal whose type is the same in every configuration keeps it (`dut.clk()` is an
`In<Bit>`). One whose width follows a parameter has an associated type instead:
`dut.count()` is an `Out<D::Count>`, `D::Count` being `Bits<3>` in `fifo_8x4` and
`Bits<5>` in `fifo_32x16`. Signals whose types agree in every configuration share one
(`wr_data` and `rd_data` are both `D::WrData`), so a value moves between them. An
associated type is bounded by `simjig::Vector` (a bit vector: `set(0x5a)`, `get() == 3`
and `.to_u128()` work at any width) or by `simjig::Value` (a struct or enum of the
configuration: `get`, `set`, `==`, `{:?}`, `Default`), so the body needs no `where`
clause. A method or const of a bound needs its trait in scope (`use simjig::Vector;` for
`.to_u128()`, `use simjig::bits::Packed;` for `D::WrData::WIDTH`), as an interface port
whose bundle differs between configurations does (its bundle trait, `<Bundle>Ports` in
the sweep's module). A signal that not every configuration has alike (a probe of one)
is left out of `Ports`; the concrete `Dut` still has it. A test of one configuration
takes its `Dut` as before (`dut: sync_fifo::fifo_8x4::Dut`).

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
