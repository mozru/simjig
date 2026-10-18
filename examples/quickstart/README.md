# quickstart

The smallest simjig project. Copy this directory to start your own.

| File | What |
|---|---|
| `rtl/inv.sv`, `rtl/flop.sv` | the designs |
| `build.rs` | one `simjig_build::Dut` per design: verilate, generate bindings, link; the flop with `.trace_fst(true)` for waves |
| `tests/inv.rs` | a combinational test: `set`, `Timer`, `get` |
| `tests/flop.rs` | a clocked test: `simjig::clock`, `rising`, `ReadOnly` |
| `tests/inv_flop.rs` | two DUTs in one test, the inverter feeding the flop |
| `.config/nextest.toml` | a wall-clock timeout for hung tests (`slow-timeout`, `terminate-after`) |

`Cargo.toml` needs `simjig` and `simjig-bits` for the tests and `simjig-build` for the
build script. The package has no library target on purpose: the build script's link
directives go to the library when there is one, and an integration test that never names
that library does not link it, so the model would be missing (see the glue notes,
`docs/notes/glue.md`). Verilator 5.052 must be on `PATH` (in this repository: `pixi run`).

```sh
pixi run cargo nextest run -p quickstart
```

Each test file names a design with `simjig::include_dut!(inv);`, which brings in the
module `inv` with `inv::Dut`: one typed handle per port (`In<Bit>` for inputs, `Out<..>`
for outputs). A `#[simjig::test]` takes the `Dut` as its argument and runs as an async
function in simulation time.

## Timeouts

A test that waits forever on a running clock never ends by itself, so each
`#[simjig::test]` has a simulation-time budget: 10 ms by default, set with
`#[simjig::test(timeout = "200us")]`, or removed with `timeout = "none"`. Past it the test
fails with a `TimeLimit` report, which says whether the budget was the default.

A hang that does not advance simulation time (a task that loops without awaiting) needs a
wall-clock limit too: `.config/nextest.toml` has nextest mark a test slow after 30 s and
kill it after 2 minutes. nextest reads that file at the root of the Cargo workspace, so
keep it there when you copy this directory.

## When a test fails

It prints one block with the failure, its triage signature and a `repro:` line, for
example `SIMJIG_SEED=0x… pixi run cargo nextest run -p quickstart -E 'binary(=flop) &
test(=…)'`. Run that line to replay the same seed; it starts with `pixi run` when the test
ran inside pixi, because a plain shell has no Verilator. The run's `summary.json` and
`events.jsonl` are in `<target>/simjig/runs/<package>/<binary>/<test>/<seed>/`.

## Waves

The flop is built with `.trace_fst(true)`, so a failing flop test can get waves. The
harness replays the run with the same seed in a child process, with FST tracing over the
end of the run, checks that the replay ended the same way, and writes `waves.fst` and a
GTKWave `waves.gtkw` (marker at the failure) into the run directory:

```sh
SIMJIG_WAVES_ON_FAIL=1 pixi run cargo nextest run -p quickstart   # waves for every failure
pixi run cargo simjig waves q_follows_d --window 10ns..           # one test, passing or not
```

The report's `waves:` line gives the command (or the files, after a replay). A model built
without `.trace_fst(true)`, like the inverter here, has none, and the line says so.

## More

The repository's `README.md` shows the rest with examples: `.timing(true)` and
`.coverage(true)`, DPI-C imports implemented in Rust (`dut.set_dpi(..)`), several DUTs
with `Lockstep` and `Mirror`, and `fork` checkpoints.
