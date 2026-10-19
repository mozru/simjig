# simjig guide

The longer user topics that the top `README.md` only touches. Each section stands on its
own.

## Tests

A `#[simjig::test]` function takes the test context first, then the DUT, and both are
optional: `async fn t(ctx: &simjig::TestCtx, dut: flop::Dut)`, `async fn t(dut: flop::Dut)`
or `async fn t()`. A test may take two to four DUTs (see "Several DUTs").

Every test has a simulation-time budget: 10 ms unless it sets `timeout = "200us"` (or
`timeout = "none"`). Past it the test fails with `TimeLimit`. A hang that does not advance
simulation time (a task that loops without awaiting) needs a wall-clock limit too: the
quickstart's `.config/nextest.toml` sets `slow-timeout` with `terminate-after`. nextest
reads that file at the root of the Cargo workspace.

`simjig::prelude` holds what nearly every testbench uses; its rustdoc lists it. `In`,
`Out` and `InOut` are not in it, because those generic names would clash with a
testbench's own under a glob import: write `use simjig::{In, Out};` in a helper that names
a handle type.

The quickstart's `Cargo.toml` has an optional lint table: the repository's lints, with
`unwrap_used`, `expect_used` and `missing_docs` allowed so tests may unwrap. Cargo cannot
extend `[lints] workspace = true` with local entries, so the table lists every lint.

## Values

`set` takes `impl Into<T>`: an integer literal (as `u128`) for any `Bit`, `Bits<N>` or
`WideBits` signal, a `bool` for a `Bit`, a value of the signal's own type, or a generated
enum constant or packed struct. A literal that does not fit panics, naming the signal and
its width. A typed integer converts with `u128::from`: `sig.set(u128::from(byte))`.

Comparisons work the same way: `x.get() == 0x12` infers the literal; a typed integer is
compared as `x.get() == u128::from(byte)`. In `assert_eq!`, prefer
`assert_eq!(x.get(), Bits::new(want))` over `assert_eq!(x.get(), want)`: the width is
inferred from the left, and a mismatch prints both sides alike (`32'h0a04a19f` vs
`32'h9fa1040a`). There is no `From<u8>` for `Bits<N>`: it would make `set(1)` ambiguous.

## Generated names

`simjig::include_dut!(fifo)` includes the module the build script generated for the DUT
`fifo`. `cargo simjig show fifo` prints it (`--path` prints only its path, `-p <crate>`
picks the package). SystemVerilog names map to Rust names like this:

| SystemVerilog | Rust |
|---|---|
| port or struct field `data_in`, `dataIn`, `DATA_IN` | field `data_in` |
| a Rust keyword: `in`, `type` | `r#in`, `r#type` (`dut.r#in.set(1)`); `self`, `super`, `crate` become `self_`, ... |
| a name starting with a digit: `2nd` | `_2nd` |
| typedef `status_t`, `op_e`, `item_s`, `word_u` | `Status`, `Op`, `Item`, `Word`: one `_t`/`_e`/`_s`/`_u` suffix dropped, then `CamelCase` |
| two typedefs that would get one name (`a::state_t`, `b::state_t`) | both keep the suffix (`StateT`), then add the scope (`AStateT`, `BStateT`), then a number |
| an anonymous struct or enum, first used by port `in_anon` | `InAnon` |
| a struct typedef `beat_t` inside an interface | `Beat`, then `UpBeat` if that name is taken |
| enum `status_t` with `ST_EMPTY`, `ST_FULL` | `Status::EMPTY`, `Status::FULL`: a prefix every variant shares is dropped, unless a remainder would start with a digit (`MODE_1` stays) |

An enum is a newtype over its bits, so it can hold values outside its variants:
`Status::EMPTY` is a constant, `s.variant()` returns `Ok(StatusVariant::Empty)` (a Rust
`enum` to `match`) or `Err(InvalidEnum)`, and `Status::from_bits`/`to_bits` convert. A
packed struct is a Rust struct with public fields (`Item { tag, data }`); a packed union
has a getter and a constructor per view (`w.bytes()`, `Word::from_bytes(..)`).

## Build options

```rust
simjig_build::Dut::new("core")
    .sources(["rtl/core.sv"])
    .top("core")
    .trace_fst(true)   // FST waves
    .timing(true)      // --timing: `#` delays and `@` events run in simulation time
    .coverage(true)    // --coverage: Sim::coverage, write_coverage, zero_coverage
    .build()?;
```

- Verilator's lint warnings fail the build. Waive one code with
  `.verilator_args(["-Wno-WIDTHEXPAND"])`. `-Wno-fatal` keeps them all as warnings, but
  also hides a port width mismatch between an interface and the module bound to it.
- File lists: `.file_list("rtl/files.f")` (`-f`, paths relative to the package root) or
  `.file_list_relative("rtl/files.F")` (`-F`, relative to the list). They are read in
  order, before `.sources([..])`, so a package listed first is declared before the
  modules that import it. An `-f` among `verilator_args` is read after the sources, which
  is too late for a package.
- Parameters: `.param("WIDTH", 16)`.
- If any source declares a `timeunit` (or `` `timescale ``), every module and package must
  (Verilator's `TIMESCALEMOD`, IEEE 1800-2023 §3.14.2.3). Add the `timeunit`, or give the
  rest a default with `.verilator_args(["--timescale", "1ns/1ps"])`.
- The model's C++ is always built with `-O2`, even in a debug build; `SIMJIG_CXX_OPT=3`
  asks for `-O3`.

### Optional DUTs

A DUT the crate can do without (one that needs a tool not every machine has) is optional:
handle its error instead of returning it, and gate its uses on `cfg(simjig_dut_<name>)`,
which `Dut::build` sets only when the DUT built.

```rust
// build.rs
if simjig_build::Dut::new("fast_model").sources(["rtl/big.sv"]).top("big").build().is_err() {
    println!("cargo::warning=fast_model skipped");
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

The `cfg` reaches the targets of the package whose build script built the DUT, not the
crates that depend on it. A script that calls `Dut::build` only sometimes must still
declare the gate when it does not:
`println!("cargo::rustc-check-cfg=cfg(simjig_dut_fast_model)");`. An ungated
`include_dut!` of a DUT that failed is a compile error naming the DUT and its error.

### Interface parameters

A Verilator top cannot have interface ports, so `.bind_iface("up", "bus_if.sink")` makes
the generated wrapper instantiate one `bus_if` per port. Its parameters follow one rule:

1. A value the binding sets wins:
   `.bind_iface_with_params("up", "bus_if.sink", [("WIDTH", 16)])`.
2. Otherwise a `.param` of the same name is the default.
3. Otherwise each side keeps its own default. When a parameter both declare ends up with
   two values, the build warns and names the `.bind_iface_with_params(..)` that settles it.

The tie is by name only: an interface port header carries no parameter assignments
(IEEE 1800-2017 §25.5), so nothing in the design says the two are one quantity.

## Parameter sweeps

A `Sweep` builds several configurations of one module, and one test body runs on each.
In `examples/lib/build.rs`:

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

A test over `D: Ports` runs once per configuration listed in `duts`
(`count_climbs_to_the_depth::fifo_8x4`, ...), from `examples/lib/tests/sync_fifo.rs`:

```rust
#[simjig::test(init = random, timeout = "2us", duts = [fifo_8x4::Dut, fifo_32x16::Dut])]
async fn count_climbs_to_the_depth<D: Ports>(dut: D) {
    clock(dut.clk(), ns(10)).start();
    dut.rst().set(1);
    cycles(dut.clk(), 2).await;
    dut.clk().falling().await;
    dut.rst().set(0);
    dut.push().set(1);
    let depth = u128::from(D::DEPTH.unsigned_abs());
    for n in 1..=depth {
        dut.wr_data().set(n); // a `u128` that fits every configuration's `WIDTH`
        dut.clk().rising().read_write().await;
        assert_eq!(dut.count().get(), n, "after {n} pushes, DEPTH {depth}");
        assert_eq!(dut.full().get(), n == depth);
        dut.clk().falling().await;
    }
}
```

The details:

- Each configuration is its own DUT. `simjig::include_dut!(sync_fifo)` includes each
  configuration's module (`sync_fifo::fifo_8x4::Dut`), the types that are the same in
  every configuration (defined once), and the trait `sync_fifo::Ports`, which every
  configuration's `Dut` implements. Its methods return the `Dut`'s fields (`dut.clk()`)
  and its consts are the parameters (`D::DEPTH`, an `i32`).
- A signal whose type depends on a parameter has an associated type (`D::Count`), bounded
  by `simjig::Vector` (a bit vector at any width), a struct's fields trait, or
  `simjig::Value`. Signals whose types agree in every configuration share one.
  `use sync_fifo::*;` brings every trait a test body needs.
- A signal that not every configuration has alike is left out of `Ports`; the concrete
  `Dut` still has it.
- A bit vector converts from `u128` only, so a `u64` from `rng.below(n)` is passed as
  `u128::from(..)`.
- In the closure, `.param` again replaces a value, but `.sources` again appends. To give
  one member other RTL, make the sources an argument of the closure.

A packed struct whose field widths follow a parameter gets a fields trait named after its
typedef (`beat_t` gets `BeatFields`): a getter per field, a constructor taking every field
in declaration order, and `with_<field>`. From `examples/lib/tests/hop_stage.rs`:

```rust
#[simjig::test(init = random, timeout = "1us", duts = [hop_8x2::Dut, hop_32x4::Dut])]
async fn a_beat_hops_with_its_tag_incremented<D: Ports>(dut: D) {
    clock(dut.clk(), ns(10)).start();
    dut.rst().set(1);
    dut.up().valid().set(0);
    dut.clk().rising().await;
    dut.clk().falling().await;
    dut.rst().set(0);
    let wrap = 1u128 << D::TAG_W;
    for (tag, data) in [(0, 0x5a), (wrap - 1, 0x3c)] {
        let beat = D::Beat::new(tag, data); // fields in declaration order
        dut.up().beat().set(beat);
        dut.up().valid().set(1);
        dut.clk().rising().read_write().await;
        let hopped: D::Beat = dut.down().beat().get(); // one type on both ports
        assert_eq!(hopped, beat.with_tag((beat.tag().to_u128() + 1) % wrap));
        assert_eq!(hopped.data(), beat.data());
        let data: D::DownData = dut.down_data().get(); // the field's type is the port's
        assert_eq!(data, hopped.data());
        assert_eq!(dut.down().valid().get(), 1);
        dut.clk().falling().await;
    }
}
```

## Random initial values

Verilator is 2-state and starts every register at 0, so a flop that reset never clears
passes every test that resets at time 0. `#[simjig::test(init = random)]` (or
`SIMJIG_INIT=random` for every test without an `init` argument) starts each variable
without an initializer from a value drawn from the seed. The same seed gives the same
start, and the `repro:` line carries `SIMJIG_INIT=random`. `$urandom` is not affected.

- In a multi-DUT test, models of one design start alike, so the two sides of a `Lockstep`
  agree at cycle 0.
- The model must keep Verilator's default `--x-initial unique`. One built with
  `.x_initial(XInitial::Zero)` or `XInitial::Fast` refuses `init = random`, and a raw
  `--x-initial` in `verilator_args` is refused.

## Waves

A run is never traced while it first runs. When you ask for waves, the harness replays the
test with the same seed in a child process, traces the end of the run, checks that the
replay ended the same way, and writes `waves.fst` and a GTKWave `waves.gtkw` (with a marker
at the failure) into the run directory. Each model and each `fork` branch gets its own
file, listed in the report.

```sh
SIMJIG_WAVES_ON_FAIL=1 cargo nextest run                       # every failure gets waves
SIMJIG_WAVES=1 SIMJIG_SEED=0x... cargo nextest run -E 'test(=m::t)'   # one run
SIMJIG_WAVES=window:2us..3us ...                               # a chosen span
cargo simjig waves m::t --seed 0x... [-p <package>] [--window 1.5us..]
```

The model must be built with `.trace_fst(true)`; otherwise the report's `waves:` line says
so. `cargo simjig waves` runs `cargo nextest run` in the current directory; arguments after
`--` go to nextest. In a synchronous test's waves, an input written between ticks shows at
the next rising edge, the eval that takes it, as in a hand-written Verilator loop.

## DPI-C imports in Rust

For `import "DPI-C" function int dpi_add(input int x, input int y)`, the generated module
has a `Dpi` trait with one method per import. Register an implementation before the first
eval:

```rust
struct Adder;

impl dpi::Dpi for Adder {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x + y
    }
}

#[simjig::test]
async fn sums(dut: dpi::Dut) {
    dut.set_dpi(Adder); // before any await
    dut.a.set(2);
    dut.b.set(3);
    timer(ns(1)).await;
    assert_eq!(dut.sum.get().to_u128(), 5);
}
```

An implementation runs inside the model's eval: it cannot read or write signals, await,
spawn, or drive another model. Record what it needs (in an `Rc<RefCell<..>>` shared with
the test) and act on it in a task. A panic fails the run as `DpiPanic`; a missing
`set_dpi` fails as `Setup`. Two instances of one DPI build cannot share a simulation:
build the design twice under two `Dut::new` names.

## Several DUTs, lockstep, fork

A test may take two to four DUTs; they share one simulation. `Mirror` drives an input of
two models at once and `Lockstep` compares outputs every cycle, reporting the first
divergent one:

```rust
#[simjig::test]
async fn rev2_matches_rev1(old: counter::Dut, new: counter_rev2::Dut) {
    clock(old.clk, ns(10)).start();
    clock(new.clk, ns(10)).start();
    Mirror::new(old.en, new.en).set(1);
    let mut lockstep = Lockstep::new().output(old.count, new.count);
    for _ in 0..100 {
        old.clk.rising().read_only().await;
        lockstep.step(); // panics with the divergence
    }
}
```

`simjig::fork::run_from_checkpoint` runs several branches from one state (after a long
reset, say), each in a forked copy of the process with its own seed (Unix, under nextest):

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

Checkpoints do not nest, and branches run one at a time; the `simjig::fork` docs list the
other limits.

## Coverage

A model built with `.coverage(true)` exposes its counters: `Sim::coverage(model)` (or
`TestCtx::coverage` in a test) reads them, `zero_coverage` clears them, and
`write_coverage(model, "coverage.dat")` writes Verilator's file for `verilator_coverage`.
`model` is 0 for the first DUT, then the test's DUT parameters in order.

## Synchronous tests

`#[simjig::test(sync)]` on a plain `fn` steps one `SyncClock`. `cyc.tick()` is one cycle
and exactly two evals: the clock rises, eval, the clock falls, eval. `rise()` and `fall()`
are the halves and `ticks(n)` is `n` cycles. A `set` goes into the model at once and the
next eval takes it; a `get` after `tick()` sees what the falling edge's eval settled.
`cyc.bind(handle)` gives a handle that reads and writes the model directly, without a
lookup; bind several at once with a tuple. `cyc.cycle()` counts rising edges.

```rust
#[simjig::test(sync)]
fn counts_bound(dut: counter::Dut) {
    let mut cyc = SyncClock::new(dut.clk, ns(10));
    let (rst, en, count) = (cyc.bind(dut.rst), cyc.bind(dut.en), cyc.bind(dut.count));
    rst.set(1);
    cyc.tick();
    rst.set(0);
    en.set(1);
    while cyc.cycle() < 100 {
        cyc.tick();
    }
    assert_eq!(count.get().to_u128(), 99);
}
```

Sweeps, `init`, the seed and `ctx.rng`, the time limit, the report and waves on failure
work as in async tests. What a synchronous test does not have:

- No scheduler: no tasks, kernel clocks, triggers, `with_timeout` or regions. `spawn` and
  `clock(..).start()` panic, naming the mode. Calling an async helper without awaiting it
  is a compile error there (`unused_must_use` is denied); `let _ = reset(dut);` is caught
  only by `cargo clippy` (`let_underscore_future`).
- No delta cycles: the model settles inside each eval.
- One clock. A second clock domain needs an async test.
- No trace hash, so the determinism check compares only how the run ended.

## Differences from cocotb

The scheduler is checked against recorded cocotb 2.1 runs; `tests/conformance/README.md`
lists every known difference with its test. The ones that change how you write a test:

- Dropping a `Task` cancels it. `spawn` returns a handle; call `.detach()` to run it in the
  background (cocotb's `start_soon`), `.await` it, or `.cancel()` it. `let _ = spawn(..)`
  drops the handle before the task ever runs, so simjig fails the run (`Setup`) instead of
  passing without it. Leaving a scope early (`?`, `return`) drops its handles too; from the
  test body that just ends the run, but inside a `scope` or a spawned task it fails as
  `Setup`.
- A `Task` passed by value to `first!`, `join!` or `with_timeout` is cancelled when the
  combinator drops it. cocotb keeps it running. Pass `&mut task` to keep it.
- Cancellation cleanup (`Drop`) runs when an operand is cancelled, one scheduling hop
  before cocotb's `finally` would.
- A `Lock` or `Queue` grant that lands on a cancelled `first!` loser is passed on to the
  next waiter; cocotb keeps the lock held or loses the item.
- A zero-tick `timer` fails when it is awaited, not when it is made.
- A timestep that never settles fails instead of hanging: more than `DELTA_MAX` (64) evals
  between `ReadWrite` rounds, or more than `READ_WRITE_MAX` (10 000) `ReadWrite` rounds.
- A timeout is a value, `Err(Elapsed)`, not an exception.
- Every test has a simulation-time budget (10 ms by default); cocotb has none.

## Performance tuning

- Build the tests with one codegen unit; the scheduler then inlines across modules, about
  10% faster on the benchmark FIFO. Profiles go in the workspace root's `Cargo.toml`:

  ```toml
  [profile.release]
  codegen-units = 1
  lto = "thin"
  ```

- On Linux the model is built without frame pointers, so profile with
  `perf record --call-graph dwarf`, or build with `CXXFLAGS=-fno-omit-frame-pointer`.
  `CXXFLAGS=-mcpu=native` is opt-in; it gained nothing measurable on the benchmark.
- While a model writes waves, every eval is sampled. Waves on failure write them only in
  the replay.
- The trace hash costs about a sixth of a busy testbench's time.
  `#[simjig::test(trace_hash = false)]` turns it off; the report then has no hash to
  compare runs by.
- `#[simjig::test(elide_evals)]` skips an eval when nothing was written since the last
  one, about 9% on the benchmark FIFO. It is off by default because it is not always
  safe: it can miss a `$display`, `$urandom` draw, `$time` read or DPI-C call in logic fed
  by an input the design writes itself (`ASSIGNIN` waived) or by `$sampled`. If your
  design does neither, it is safe. When unsure, leave it off.
  `ModelOptions::with_elide_evals` has the details.
