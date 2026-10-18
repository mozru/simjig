# simjig vs cocotb: a sync_fifo testbench

How fast simjig runs a testbench-heavy simulation, compared with cocotb running the same
testbench on the same RTL, with the same Verilator. Not part of `cargo xtask ci`: nothing
here runs unless you run it.

```sh
bench/run.sh                 # everything below: env, build, bug check, measurements, table
bench/run.sh build           # build both testbenches only
bench/run.sh run --sizes 10000 --runs 1 --max-load 100 --out target/smoke.jsonl  # a smoke run
bench/run.sh table bench/results/raw.jsonl
```

| File | What |
|---|---|
| `simjig/` | the simjig testbench: a standalone Cargo workspace (`build.rs`, `tests/bench.rs`) |
| `cocotb/test_bench.py` | the cocotb testbench, statement for statement the same |
| `cocotb/run.py` | builds and runs it with cocotb's Verilator runner |
| `bench.py` | the driver: build, bug check, measured runs, table |
| `results/` | the committed results: `env.txt`, `bug.txt`, `raw.jsonl` (one line per run), `table.md` |

## Results

RESULTS_PLACEHOLDER

## Method

**DUT.** `examples/lib/rtl/sync_fifo.sv` with `WIDTH=8`, `DEPTH=16`, `FWFT=0` (registered
read), verilated by both flows with the same Verilator 5.052 (pixi). No tracing, no
coverage, no `--timing`, no `-O` (Verilator's default optimisation in both), one thread.
The exact Verilator command lines (from each build's `__verFiles.dat`):

- simjig (`bench/simjig/build.rs`, model `fifo`):
  `verilator -Mdir <cache>/obj --cc --MMD --prefix Vfifo_ac373c63 --no-timing
  --top-module sync_fifo -GWIDTH=8 -GDEPTH=16 -GFWFT=0 <cache>/Vfifo_ac373c63.vlt
  examples/lib/rtl/sync_fifo.sv`. The `.vlt` file marks the ports public. The model
  `fifo_pub` adds `--public-flat-rw`.
- cocotb (`cocotb_tools.runner`, Verilator): `verilator -cc --exe -Mdir
  target/bench-cocotb --top-module sync_fifo --vpi --public-flat-rw --prefix Vtop -o
  sync_fifo -LDFLAGS "-Wl,-rpath,<cocotb>/libs -L<cocotb>/libs -lcocotbvpi_verilator"
  --timescale 1ns/1ps -GWIDTH=8 -GDEPTH=16 -GFWFT=0 <cocotb>/share/lib/verilator/verilator.cpp
  examples/lib/rtl/sync_fifo.sv`, then `make -j -C target/bench-cocotb -f Vtop.mk VM_TRACE=0`.

The differences are the ones each flow needs: cocotb's `--vpi --public-flat-rw` (VPI
access) and its `main` (`verilator.cpp`); simjig's `--no-timing` and its `.vlt` (the
design has no delays, so `--no-timing` changes nothing, and the timescale is the RTL's own
`timeunit 1ns; timeprecision 1ps`). The `fifo_pub` rows measure what `--public-flat-rw`
costs a simjig model, so the model cost can be compared like for like.

**C++.** Both compile with the pinned clang 21 at `-O2`: simjig-build's default
(`SIMJIG_CXX_OPT`), and for cocotb `OPT_FAST=-O2 OPT_SLOW=-O2 OPT_GLOBAL=-O2` (through
`MAKEFLAGS`) instead of `verilated.mk`'s `-Os`. Both also get conda's `CXXFLAGS`
(`-O2 -ftree-vectorize -fstack-protector-strong -D_FORTIFY_SOURCE=2 ...`). The Rust is
built with `cargo test --profile bench` (release, plus line tables).

**Testbench** (identical in both; `tests/bench.rs` and `cocotb/test_bench.py`):
- a 10 ns clock from the simulator's own clock: simjig's kernel clock
  (`clock(clk, ns(10)).start()`), and cocotb's `Clock`, which with
  `COCOTB_TRUST_INERTIAL_WRITES=1` is the C++ GPI clock (`impl="gpi"`, recorded in every
  cocotb result line);
- drivers write at the falling edge, the monitor samples at the rising edge (both flows
  show pre-edge values there: `tests/conformance/README.md`);
- **(A) `clock_only`**: start the clock, await N rising edges;
- **(B) `monitor_only`**: A plus a monitor task that reads `push`, `pop`, `full`, `empty`,
  `count`, `rd_data` at each rising edge, converts them to integers and folds them into a
  hash;
- **(C) `full`**: reset (two edges), then a producer (each falling edge: push with the
  phase's rate, a random byte on `wr_data`), a consumer (each falling edge: pop with its
  rate) and a monitor that at each rising edge reads 7 signals (B's six plus `wr_data`,
  which the model needs) and checks `count`, `full`, `empty` and `rd_data` against a
  reference model, then applies the edge's push and pop to it. Rates are redrawn every 24
  cycles from {10, 50, 90} %, so the FIFO fills and drains.
- Both draw from the same xorshift32 (seeds `0x12345678`, `0x9E3779B9`), so both simulate
  the very same stimulus: every result line carries the pushes, pops and a hash of the
  popped bytes (B: of the sampled values), and `table` reports any mismatch. There was
  none.
- In cocotb, signal handles and trigger objects are looked up once, outside the loops.

**Timing.** Each testbench times its own body (from the first statement to the end of
the last task), so model construction, Python start-up, cocotb's regression setup and
the harness's report are excluded; `process s` is the wall time of the whole process
(simjig: the test binary running one test; cocotb: the runner's simulator process), for
reference. Builds are never timed. Each point is 5 runs, each in a fresh process, run one
at a time; the table gives the median, min and max of the in-testbench time and
cycles/s = N / median. Speedup = cocotb median / simjig median.

**Idle machine.** Before every batch (one flow, variant and size: 5 runs), the driver
waits until the 1-minute load average is below 1.0 and no `cargo`, `rustc`, `clang`,
`verilator` or `nextest` process is running; the load it saw is in each result line and
the table's `load` column. Nothing is built while measuring.

**Correctness check.** `bench/run.sh bug` deletes `|| do_pop` from the push condition
(a full FIFO then drops a push that comes with a pop) and runs C for 10 000 cycles on
both. Both fail at the same check (`results/bug.txt`): `count` is 15 where the model
holds 16.

## Caveats

- One design, one size: a tiny FIFO, so the model's eval is cheap and the numbers are
  dominated by the testbench and the scheduler. A large design narrows the gap, since
  both flows spend the same time in Verilator's eval.
- The stimulus RNG is a hand-written xorshift32, not `ctx.rng` or Python's `random`: the
  price of identical stimulus. It costs cocotb roughly 3 Python calls per cycle in C.
- cocotb's assertions are rewritten by pytest (installed in the env, cocotb's default
  `COCOTB_REWRITE_ASSERTION_FILES=*.py`): 4 asserts per cycle in C, a little slower than
  plain `assert`. simjig's `assert_eq!` formats only on failure.
- C's monitor reads 7 signals, B's 6. simjig's harness also records a trace hash of
  every scheduler event (always on); that is part of what is measured.
- Apple M6, macOS; other machines and Linux will differ.
