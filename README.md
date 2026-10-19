# simjig

simjig lets you write testbenches for Verilog and SystemVerilog in Rust, run against a
Verilator model in the same process. Tests are async functions with cocotb's scheduling
(edges, timers, `ReadWrite`/`ReadOnly`, fork/join), and ports, structs and enums from your
RTL become Rust types. Tests are seeded and print a one-line repro when they fail.
simjig is pre-release and not on crates.io; CI runs on macOS arm64 and Linux aarch64.

```rust
// tests/flop.rs
#[simjig::test(timeout = "1us")]
async fn q_follows_d(dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    reset(&dut).await;
    for d in [true, false, true] {
        dut.d.set(d);
        // After `.read_write()` the flop shows what the edge captured.
        dut.clk.rising().read_write().await;
        assert_eq!(dut.q.get(), d);
    }
}
```

## Install

You need Verilator, a C++ compiler, cargo-nextest and Rust 1.99 or newer. simjig checks
for exactly Verilator 5.052. The pixi environment has Verilator and the compiler;
run cargo through `pixi run`:

```sh
pixi install --locked
pixi run cargo install --locked cargo-nextest
pixi run cargo install --locked --path crates/cargo-simjig   # the `cargo simjig` command
pixi run cargo simjig doctor                                 # checks the setup
pixi run cargo nextest run -p quickstart
```

Without pixi, put Verilator on `PATH` or set `VERILATOR_ROOT`. Keep spaces out of the
checkout path; Verilator's launcher breaks on them.

## Your first test

Copy [`examples/quickstart`](examples/quickstart) and replace the top of its `Cargo.toml`
(the `.workspace = true` keys only work inside this repository). Until simjig is on
crates.io, depend on a checkout, `<simjig>` here:

```toml
[package]
name = "my_tb"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
simjig = { path = "<simjig>/crates/simjig" }

[build-dependencies]
simjig-build = { path = "<simjig>/crates/simjig-build" }
```

In the tests, rename `quickstart` in `use quickstart::*;` to `my_tb`.

The build script verilates each design and generates a Rust module for it:

```rust
// build.rs
fn main() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("inv")
        .sources(["rtl/inv.sv"])
        .top("inv")
        .build()?;
    simjig_build::Dut::new("flop")
        .sources(["rtl/flop.sv"])
        .top("flop")
        .trace_fst(true) // FST waves, for waves on failure
        .build()?;
    Ok(())
}
```

The library includes those modules and holds shared helpers, so each test file starts with
one `use`:

```rust
// src/lib.rs
pub use simjig::prelude::*;
simjig::include_dut!(inv); // the module `build.rs` generated: `inv::Dut`, one handle per port
simjig::include_dut!(flop);

/// Holds the flop in reset for one rising edge, and releases it right after that edge.
pub async fn reset(dut: &flop::Dut) {
    dut.rst.set(1);
    dut.clk.rising().read_write().await;
    dut.rst.set(0);
}
```

Then `cargo nextest run` runs the test at the top of this page. Outside this repository,
run cargo with `pixi run --manifest-path <simjig>/pixi.toml` (see the quickstart README).

## Driving and checking signals

`dut.x.set(v)` writes and `dut.x.get()` reads. `set` takes an integer literal, a `bool`
for a 1-bit signal, or a value of the signal's type; a literal that does not fit panics.
A write is applied at the next eval, so a read right after `set` returns the old value.
Values are `Bit`, `Bits<N>` (up to 128 bits) and `WideBits`. They compare with literals
(`dut.count.get() == 3`); typed integers go through `u128::from(x)`. In `assert_eq!`, put
`Bits::new(want)` on the right so a mismatch prints both sides in hex.

Packed structs become Rust structs; enums become newtypes with a constant per variant
(`Status::EMPTY`; `s.variant()` to `match`). `cargo simjig show flop` prints exactly what
was generated.

## Clocks and time

`clock(dut.clk, ns(10)).start()` drives a clock, `cycles(dut.clk, n).await` waits for `n`
rising edges, and `timer(ns(5)).await` lets time pass. At a rising edge, a test can look
at the design at three moments:

| Await | You see | Writes |
|---|---|---|
| `dut.clk.rising()` | pre-edge values: what the flops capture | allowed; the next edge takes them |
| `dut.clk.rising().read_write()` | the flops' new values, logic settled | allowed |
| `dut.clk.rising().read_only()` | final values for this timestep | fail the test |

A driver writes after `rising()` or `.read_write()`; a monitor samples in `.read_only()`.
`read_write()` and `read_only()` also work alone, for the current timestep. A timer ending
on an edge's timestep wakes before the edge.

## Concurrency

`spawn("name", fut)` starts a task and returns a handle to `.await`, `.cancel()` or
`.detach()` (run in the background). `first!(a, b)` waits for the first future and
`join!(a, b)` for all; `with_timeout(us(2), fut)` gives `Err(Elapsed)` if time runs out.
`Queue` and `Event` connect tasks, and `ctx.rng("name")` is a seeded random stream.

```rust
// tests/scoreboard.rs
#[simjig::test(timeout = "10us")]
async fn q_follows_random_d(ctx: &TestCtx, dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    reset(&dut).await;
    let sb = Queue::new("sb", 0); // 0: unbounded
    let expected = sb.clone();
    let mut rng = ctx.rng("stim"); // seeded: the report's seed replays it
    let driver = spawn("driver", async move {
        for _ in 0..100 {
            dut.d.set(rng.bool());
            dut.clk.rising().await; // the edge captures `d`
            expected.put(dut.d.get()).await;
        }
    });
    spawn("monitor", async move {
        loop {
            dut.clk.rising().read_only().await; // final values of the edge's timestep
            assert_eq!(dut.q.get(), sb.get().await);
        }
    })
    .detach(); // runs until the test ends
    with_timeout(us(2), driver).await.unwrap();
}
```

## When a test fails

```text
FAIL q_follows_random_d  seed=0x5502aca5e1b140ed  t=10 ns  [TaskPanic sig=c470890eb03c84d1]
  task:     main.monitor
  at:       examples/quickstart/tests/scoreboard.rs:23:13
  message:  panicked: assertion `left == right` failed
  repro:    SIMJIG_SEED=0x5502aca5e1b140ed pixi run cargo nextest run -p quickstart -E '...'
  waves:    SIMJIG_WAVES=1 SIMJIG_SEED=0x5502aca5e1b140ed pixi run cargo nextest run ...
```

Paste the `repro:` line to rerun the same seed; runs are deterministic. With
`.trace_fst(true)` and `SIMJIG_WAVES_ON_FAIL=1`, a failing test is replayed into
`waves.fst` and `waves.gtkw` in the run directory (`cargo simjig waves <test> --seed <s>`
for one test). A test fails after 10 ms of simulated time; change it with
`timeout = "200us"` or `"none"`.

## Parameter sweeps and random init

A `Sweep` in `build.rs` builds several configurations of one module. Here `fifo(..)` is a
small closure in `examples/lib/build.rs` that returns a `Dut` with a name, width and depth:

```rust
Sweep::new("sync_fifo")
    .dut(fifo("fifo_8x4", 8, 4))
    .dut(fifo("fifo_32x16", 32, 16))
    .build()?;
```

A test generic over `D: sync_fifo::Ports` with `duts = [fifo_8x4::Dut, fifo_32x16::Dut]`
runs once per configuration, with signals as methods (`dut.clk()`) and parameters as
consts (`D::DEPTH`); `docs/guide.md` has an example.

`#[simjig::test(init = random)]` starts every register from a seeded random value instead
of 0, which catches state that reset forgets. `SIMJIG_INIT=random` does it for all tests.

## Sync mode for fast loops

For a loop that only drives, ticks and checks, a test can be a plain `fn` with one
`SyncClock`. It runs close to a hand-written C++ loop but has no tasks, triggers or
regions; use async tests for concurrent drivers and monitors, or several clocks.

```rust
// tests/flop_sync.rs
#[simjig::test(sync)]
fn q_follows_d_sync(dut: flop::Dut) {
    let mut cyc = SyncClock::new(dut.clk, ns(10));
    let q = cyc.bind(dut.q); // a bound handle: faster reads in the loop
    for d in [true, false, true] {
        dut.d.set(d);
        cyc.tick(); // rise, eval, fall, eval
        assert_eq!(q.get(), d);
    }
}
```

## Coming from cocotb

cocotb's trigger classes are snake_case functions here: `Timer(...)` becomes `timer(...)`.

| cocotb | simjig |
|---|---|
| `Clock(dut.clk, 10, "ns").start()` | `clock(dut.clk, ns(10)).start()` |
| `await RisingEdge(dut.clk)`, `FallingEdge`, `ValueChange` | `dut.clk.rising().await`, `.falling()`, `.changed()` |
| `await ClockCycles(dut.clk, 3)` | `cycles(dut.clk, 3).await` |
| `await Timer(10, "ns")` | `timer(ns(10)).await` |
| `await ReadOnly()`, `ReadWrite()`, `NextTimeStep()` | `read_only().await`, `read_write().await`, `next_time_step().await` |
| `cocotb.start_soon(coro)` | `spawn("name", fut).detach()` |
| `await First(a, b)`, `Combine(a, b)` | `first!(a, b)`, `join!(a, b)` |
| `await with_timeout(x, 10, "ns")` | `with_timeout(ns(10), x).await` |
| `dut.x.value = 5`, `int(dut.x.value)` | `dut.x.set(5)`, `dut.x.get().to_u128()` |
| `dut._log.info(...)` | `tb_log!("x={x}")` |
| `Event`, `Queue`, `Lock` | `Event`, `Queue`, `simjig::Lock` |

- `first!`/`join!` await their operands themselves. `first!` needs operands with one output
  type; wrap them in `async { …; Enum::X }`.
- `let _ = spawn(..)` drops the handle and fails the test; use `.detach()`.
- A `Task` passed by value to `first!` or `with_timeout` is cancelled when it loses. Pass
  `&mut task` to keep it running.
- A write in `ReadOnly` fails the test. A timeout is a value, not an exception.
- More differences are in `docs/guide.md`. `tests/conformance/README.md` lists each
  difference with its test.

## Performance

These numbers are for the full testbench: reset, a producer, a consumer and a checking
monitor. Async simjig is about 27x faster than cocotb on a tiny FIFO, where the scheduler
dominates, and 7.4x on a mid-size design, where Verilator's eval does. Lighter testbenches
(a clock alone, or a clock and a monitor) are 8.5-21x faster than cocotb on the tiny FIFO.
Async is slower than a hand-written C++ loop: about 8.6x on the tiny FIFO and 1.2x on the
mid-size design. Sync mode is within 1.2x of the C++ loop. Median ns per cycle, 1M cycles,
Apple M6:

| design | simjig | simjig sync | C++ | cocotb |
|---|---:|---:|---:|---:|
| tiny FIFO | 298 | 42 | 35 | 8,166 |
| mid-size pipeline | 1,508 | 1,247 | 1,240 | 11,205 |

Method, caveats and numbers for Marlin (another Rust Verilator binding):
[`bench/README.md`](bench/README.md).

## More docs

- [`examples/`](examples): the quickstart, and `examples/lib` (FIFOs, a skid buffer, sweeps).
- [`docs/guide.md`](docs/guide.md): generated names, build options, interfaces, sweeps,
  waves, DPI-C, several DUTs, fork, coverage, sync mode, cocotb differences, tuning.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) to work on simjig; `ARCHITECTURE.md` for its design.

## License

[Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. Contributions are
dual licensed the same way.
