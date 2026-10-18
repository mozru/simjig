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

```sh
pixi install --locked                       # Verilator 5.052 and the C++ toolchain
pixi run cargo nextest run -p quickstart
```

A failing test prints a report with the seed, the simulation time, the failing task and a
one-line `repro:` command that reruns it with the same seed.

Every `#[simjig::test]` has a simulation-time budget: 10 ms unless it sets
`timeout = "..."` (or `timeout = "none"`). For hangs that do not advance simulation time,
the quickstart's `.config/nextest.toml` sets a wall-clock `slow-timeout` with
`terminate-after`.

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

### Coverage
A model built with `.coverage(true)` exposes its counters: `Sim::coverage(model)` (or
`TestCtx::coverage` inside a test) reads them, `zero_coverage` clears them, and
`write_coverage(model, "coverage.dat")` writes Verilator's file with every point's name,
for `verilator_coverage`. `model` is the model index: 0 for the first DUT, then the
test's DUT parameters in order.
