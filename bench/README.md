# simjig vs cocotb, Marlin and raw C++: a FIFO testbench, on a tiny and a mid-size design

How fast simjig runs a testbench-heavy simulation, compared with the same testbench on
the same RTL, with the same Verilator, in three other flows:
- **cocotb** 2.1.0: the event-driven Python reference;
- **Marlin** 0.21.1 ([crates.io](https://crates.io/crates/marlin), Rust): the model as a
  struct, driven by a hand-written loop of `eval()` calls;
- **raw C++**: a hand-written Verilator `main`, the ceiling (only Verilator and a loop).

Not part of `cargo xtask ci`: nothing here runs unless you run it.

```sh
bench/run.sh                 # everything below: env, build, bug check, measurements, table
bench/run.sh build           # build every testbench only
bench/run.sh run --sizes 10000 --runs 1 --max-load 100 --out target/smoke.jsonl  # a smoke run
bench/run.sh run --flows simjig,marlin,cpp ...  # some flows (simjig, simjig_pub, cocotb, marlin, cpp)
bench/run.sh table bench/results/raw.jsonl
bench/run.sh --design mid    # the same sequence for the mid-size design: bench/results/mid/
bench/run.sh --design mid run --sizes 10000 --runs 1 --max-load 100 --out target/smoke-mid.jsonl
```

`--design` (before or after the subcommand) picks the design: `fifo` (the default) or
`mid` (see Designs).

| File | What |
|---|---|
| `rtl/mid_pipe.sv` | the mid-size design (see Designs) |
| `simjig/` | the simjig testbenches: a standalone Cargo workspace (`build.rs`; `tests/bench.rs` for the FIFO, `tests/mid.rs` for `mid`, `tests/common/` shared) |
| `cocotb/test_bench.py`, `cocotb/test_mid.py` | the cocotb testbenches, statement for statement the same (`bench_common.py` shared) |
| `cocotb/run.py` | builds and runs them with cocotb's Verilator runner; its `DESIGNS` is each design's sources, top and parameters |
| `marlin/` | the Marlin testbenches: a standalone Cargo workspace (`tests/bench.rs`, `tests/mid.rs`; `rtl/*_bench.sv`, the parameter wrappers Marlin needs) |
| `cpp/main.cpp`, `cpp/mid.cpp` | the raw C++ testbenches (`common.h` shared) |
| `bench.py` | the driver: build, bug check, measured runs, table; its `DESIGNS` is each design's testbenches and injected bug |
| `results/` | the committed results: `env.txt`, `bug.txt`, `raw.jsonl` (one line per run), `table.md`; `results/mid/` the same for `mid` |

## Designs

- **`fifo`**: `examples/lib/rtl/sync_fifo.sv`, 8 bits by 16 entries. Tiny: Verilator's
  eval is about 7% of simjig's time (`docs/notes/bench.md`), so it measures the
  frameworks, and overstates the gap a user with a real design sees.
- **`mid`**: `bench/rtl/mid_pipe.sv` (written for this benchmark; no third-party code).
  A 64-bit `sync_fifo` of 256 entries feeding a 128-stage pipeline: about 12 400 flops
  (128 stages of a valid bit, 64 data bits and a 32-bit CRC) plus the FIFO's 16 Kib of
  storage. Each stage is a 64-bit multiply, a rotate and an xor-shift, plus a bit-serial
  CRC-32 step over its 64-bit input (64 dependent steps of combinational depth). Stages
  come in pairs that undo each other (an odd multiplier and its inverse mod 2^64), so
  `out_data` is the popped entry again, `STAGES + 1` samples after its pop: the
  scoreboard is a queue, and the testbench stays as small as the FIFO's. A cycle of the
  model costs about 1.3 us (the C++ flow's B and C at 10 000 cycles: a smoke run on a
  loaded machine, not a measurement), inside the 0.5 to 5 us aimed at; `STAGES` scales
  it about linearly (32 stages: about 0.5 us, 64: 0.7 us).

  The port is the FIFO's push side (`push`, `wr_data`) and `pop`, plus `full`, `empty`,
  `count`, and the pipeline's `out_valid`, `out_data`, `out_crc`. The same three
  variants: A the clock alone; B a monitor reading 7 signals per rising edge (`push`,
  `pop`, `full`, `empty`, `count`, `out_valid`, `out_data`) into a hash; C reset, the
  same producer (now a 64-bit word from two draws, high half first) and consumer, and a
  monitor that checks `count`, `full`, `empty` and `out_valid` every cycle, and
  `out_data` (and reads `out_crc`) whenever an entry is due. Its result is the pushes,
  the pops and a hash of every `out_crc`, which depends on every stage's intermediate
  value: every flow must report the same.

  Its injected bug (`bench/run.sh --design mid bug`) removes `&& !empty` from stage 0's
  valid: a pop of an empty FIFO then sends a stale entry down the pipeline. Every flow
  catches it at the same check, `out_valid` high where no entry is due (C++ reports
  cycle 130: the first empty pop's stale entry reaching the end).

**The results below are the FIFO's, and predate the Marlin and C++ flows**: they are simjig and cocotb only
(measured 2026-10-18). The four-flow measurement replaces them.

## Results

Apple M6, 16 GiB, macOS 27.0.1; Python 3.12.15, cocotb 2.1.0, Verilator 5.052, rustc
1.99.1, clang 21.1.8 (`results/env.txt`). Measured 2026-10-18; 5 runs per point;
`seconds` is the in-testbench time (see Method), `process s` the whole process.

Summary at 1M cycles (median in-testbench time per cycle):

| variant | simjig | cocotb | speedup |
|---|---:|---:|---:|
| A `clock_only` | 0.62 us | 1.33 us | 2.1x |
| B `monitor_only` | 0.65 us | 3.54 us | 5.5x |
| C `full` | 1.02 us | 7.97 us | 7.8x |
| C minus A (the testbench's own cost) | 0.40 us | 6.64 us | 16.6x |

- Throughput is flat in N for both flows (1.0M vs 125k cycles/s for C from 10k to 10M):
  both are linear, so the 10M cocotb points are measured, not extrapolated, and agree
  with the trend.
- Runs are tight: max/min is at most 4.5% at every point, and at most 2.8% at every
  point of 100k cycles or more but one (`fifo_pub` B at 1M: one run 4.5% slow).
- `--public-flat-rw` (`fifo_pub`) costs simjig about 1%: the model is not where the
  difference comes from.
- Every simjig and cocotb run of a point reported the same pushes, pops and hash.

Full table (`results/table.md`, from `results/raw.jsonl`):

| variant | cycles | flow | runs | median s | min s | max s | cycles/s (median) | process s (median) | speedup vs cocotb | load |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| clock_only | 10,000 | simjig | 5 | 0.0063 | 0.0062 | 0.0063 | 1,590,837 | 0.011 | 2.1x | 1.65 |
| clock_only | 10,000 | simjig (fifo_pub) | 5 | 0.0063 | 0.0062 | 0.0064 | 1,590,078 | 0.011 | 2.1x | 1.65 |
| clock_only | 10,000 | cocotb | 5 | 0.0132 | 0.0130 | 0.0135 | 758,840 | 0.230 |  | 1.65 |
| clock_only | 100,000 | simjig | 5 | 0.0623 | 0.0621 | 0.0623 | 1,606,245 | 0.067 | 2.1x | 1.68 |
| clock_only | 100,000 | simjig (fifo_pub) | 5 | 0.0631 | 0.0629 | 0.0633 | 1,585,993 | 0.068 | 2.1x | 1.68 |
| clock_only | 100,000 | cocotb | 5 | 0.1318 | 0.1298 | 0.1335 | 758,691 | 0.349 |  | 1.68 |
| clock_only | 1,000,000 | simjig | 5 | 0.6226 | 0.6203 | 0.6250 | 1,606,114 | 0.628 | 2.1x | 1.57 |
| clock_only | 1,000,000 | simjig (fifo_pub) | 5 | 0.6310 | 0.6299 | 0.6340 | 1,584,751 | 0.636 | 2.1x | 1.69 |
| clock_only | 1,000,000 | cocotb | 5 | 1.3322 | 1.3247 | 1.3472 | 750,642 | 1.551 |  | 1.69 |
| clock_only | 10,000,000 | simjig | 5 | 6.3089 | 6.2952 | 6.3264 | 1,585,062 | 6.314 | 2.1x | 1.50 |
| clock_only | 10,000,000 | simjig (fifo_pub) | 5 | 6.3976 | 6.3827 | 6.4120 | 1,563,082 | 6.403 | 2.1x | 1.57 |
| clock_only | 10,000,000 | cocotb | 5 | 13.5458 | 13.4658 | 13.7272 | 738,238 | 13.769 |  | 1.43 |
| monitor_only | 10,000 | simjig | 5 | 0.0065 | 0.0064 | 0.0067 | 1,528,818 | 0.011 | 5.3x | 1.65 |
| monitor_only | 10,000 | simjig (fifo_pub) | 5 | 0.0065 | 0.0065 | 0.0066 | 1,537,752 | 0.011 | 5.3x | 1.65 |
| monitor_only | 10,000 | cocotb | 5 | 0.0346 | 0.0343 | 0.0347 | 289,084 | 0.252 |  | 1.65 |
| monitor_only | 100,000 | simjig | 5 | 0.0645 | 0.0643 | 0.0646 | 1,550,219 | 0.069 | 5.4x | 1.68 |
| monitor_only | 100,000 | simjig (fifo_pub) | 5 | 0.0657 | 0.0653 | 0.0671 | 1,522,997 | 0.071 | 5.3x | 1.68 |
| monitor_only | 100,000 | cocotb | 5 | 0.3485 | 0.3447 | 0.3530 | 286,970 | 0.566 |  | 1.68 |
| monitor_only | 1,000,000 | simjig | 5 | 0.6471 | 0.6457 | 0.6489 | 1,545,292 | 0.652 | 5.5x | 1.66 |
| monitor_only | 1,000,000 | simjig (fifo_pub) | 5 | 0.6555 | 0.6528 | 0.6824 | 1,525,660 | 0.661 | 5.4x | 1.66 |
| monitor_only | 1,000,000 | cocotb | 5 | 3.5437 | 3.4934 | 3.5531 | 282,194 | 3.763 |  | 1.61 |
| monitor_only | 10,000,000 | simjig | 5 | 6.6422 | 6.6382 | 6.6574 | 1,505,517 | 6.648 | 5.4x | 1.14 |
| monitor_only | 10,000,000 | simjig (fifo_pub) | 5 | 6.7227 | 6.6974 | 6.7658 | 1,487,490 | 6.728 | 5.3x | 1.35 |
| monitor_only | 10,000,000 | cocotb | 5 | 35.5723 | 35.3541 | 36.0432 | 281,117 | 35.798 |  | 1.26 |
| full | 10,000 | simjig | 5 | 0.0102 | 0.0101 | 0.0103 | 977,231 | 0.015 | 7.7x | 1.65 |
| full | 10,000 | simjig (fifo_pub) | 5 | 0.0103 | 0.0102 | 0.0104 | 970,214 | 0.015 | 7.6x | 1.65 |
| full | 10,000 | cocotb | 5 | 0.0788 | 0.0778 | 0.0789 | 126,871 | 0.296 |  | 1.65 |
| full | 100,000 | simjig | 5 | 0.1020 | 0.1017 | 0.1024 | 980,709 | 0.107 | 7.7x | 1.62 |
| full | 100,000 | simjig (fifo_pub) | 5 | 0.1033 | 0.1028 | 0.1038 | 967,745 | 0.108 | 7.6x | 1.62 |
| full | 100,000 | cocotb | 5 | 0.7841 | 0.7735 | 0.7894 | 127,541 | 1.001 |  | 1.62 |
| full | 1,000,000 | simjig | 5 | 1.0232 | 1.0196 | 1.0236 | 977,349 | 1.029 | 7.8x | 1.51 |
| full | 1,000,000 | simjig (fifo_pub) | 5 | 1.0327 | 1.0298 | 1.0371 | 968,348 | 1.038 | 7.7x | 1.87 |
| full | 1,000,000 | cocotb | 5 | 7.9703 | 7.9047 | 8.0255 | 125,465 | 8.192 |  | 1.80 |
| full | 10,000,000 | simjig | 5 | 10.5165 | 10.4991 | 10.5237 | 950,885 | 10.522 | 7.6x | 1.96 |
| full | 10,000,000 | simjig (fifo_pub) | 5 | 10.5957 | 10.5909 | 10.6635 | 943,778 | 10.601 | 7.5x | 1.53 |
| full | 10,000,000 | cocotb | 5 | 79.9212 | 79.7995 | 81.0983 | 125,123 | 80.150 |  | 1.75 |

`load` is the highest 1-minute load average seen before the point's batches. The
machine's floor during measurement was about 1.9, from the Claude app (about 30% of one
core) and Spotlight indexing, with every other agent paused: on a many-core M6 running
single-threaded benchmarks, the gate was set to < 2.5 (no batch had to wait; the loads seen were 1.14 to 1.96), and no
build process ran during any batch.

## Method

**DUT.** `examples/lib/rtl/sync_fifo.sv` with `WIDTH=8`, `DEPTH=16`, `FWFT=0` (registered
read), verilated by every flow with the same Verilator 5.052 (pixi; Marlin 0.21.1
supports 5.025 and later). No tracing, no coverage, no `--timing`, no `-O` (Verilator's
default optimisation in all), one thread.
The exact Verilator command lines (from each build's `__verFiles.dat`) are below for the
FIFO; `mid`'s are the same with `--top-module mid_pipe` (Marlin: `mid_pipe_bench`), the
sources `bench/rtl/mid_pipe.sv examples/lib/rtl/sync_fifo.sv`, and no `-G` (its
parameters are its defaults, which Marlin's wrapper repeats as literals):

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

- Marlin (`VerilatorRuntime`, at the model's first use; `bench.py build` triggers it):
  `verilator --cc -sv -j 0 --build -CFLAGS "-shared -fpic -std=c++14 -DVL_TIME_CONTEXT"
  -MAKEFLAGS CXX=c++ --Mdir <artifacts>/obj_dir --top-module sync_fifo_bench
  bench/marlin/rtl/sync_fifo_bench.sv examples/lib/rtl/sync_fifo.sv ../ffi/ffi.cpp`, then
  `c++ -shared` of the two archives into a `.so` that the test `dlopen`s. Marlin has no
  way to pass parameters (its `#[verilog]` macro reads port widths from the top module's
  source), so `sync_fifo_bench.sv` instantiates `sync_fifo #(8, 16, 0)` with literal port
  widths; Verilator inlines the wrapper.
- C++ (`bench.py`, `build_cpp`): `verilator --cc --exe --build -j 0 --no-timing
  --top-module sync_fifo -GWIDTH=8 -GDEPTH=16 -GFWFT=0 -Mdir target/bench-cpp -o bench_cpp
  examples/lib/rtl/sync_fifo.sv bench/cpp/main.cpp`.

The differences are the ones each flow needs: cocotb's `--vpi --public-flat-rw` (VPI
access) and its `main` (`verilator.cpp`); simjig's `--no-timing` and its `.vlt` (the
design has no delays, so `--no-timing` changes nothing, and the timescale is the RTL's own
`timeunit 1ns; timeprecision 1ps`); Marlin's wrapper, `-sv` (the files are `.sv` anyway),
C++14, `-fpic` and its `ffi.cpp` (one `extern "C"` function per port and for `eval`).
None passes `-O` or `--trace`. The `fifo_pub` rows measure what `--public-flat-rw` costs a
simjig model, so the model cost can be compared like for like.

**C++.** All four compile with the pinned clang 21 at `-O2`: simjig-build's default
(`SIMJIG_CXX_OPT`), and for cocotb, Marlin and the C++ flow `OPT_FAST=-O2 OPT_SLOW=-O2
OPT_GLOBAL=-O2` (through `MAKEFLAGS`, which the `make` that Verilator's `--build` runs
reads too) instead of `verilated.mk`'s `-Os`. All also get conda's `CXXFLAGS`
(`-O2 -ftree-vectorize -fstack-protector-strong -D_FORTIFY_SOURCE=2 ...`). The Rust is
built with `cargo test --profile bench` (release, plus line tables). Marlin labels its
build "`verilator-O0` profile [unoptimized]": that is Marlin's name for "no `-O` passed
to Verilator", Verilator's default optimisation, as in every flow here.

**Testbench** (the same in all four: `simjig/tests/bench.rs`, `cocotb/test_bench.py`,
`marlin/tests/bench.rs`, `cpp/main.cpp`; the hand-written loops of Marlin and C++ are
described after this list):
- a 10 ns clock from the simulator's own clock: simjig's kernel clock
  (`clock(clk, ns(10)).start()`), and cocotb's `Clock`, which with
  `COCOTB_TRUST_INERTIAL_WRITES=1` is the C++ GPI clock (`impl="gpi"`, recorded in every
  cocotb result line);
- drivers write at the falling edge, the monitor samples at the rising edge (every flow
  shows pre-edge values there: `tests/conformance/README.md`);
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
- All draw from the same xorshift32 (seeds `0x12345678`, `0x9E3779B9`), so all simulate
  the very same stimulus: every result line carries the pushes, pops and a hash of the
  popped bytes (B: of the sampled values), and `table` reports any point where the flows
  disagree.
- In cocotb, signal handles and trigger objects are looked up once, outside the loops.

**Marlin and C++: a loop, not a scheduler.** Neither has tasks, triggers, delta cycles or
an event queue; the testbench owns the loop and calls `eval()` itself, so they measure
close to the raw cost of the model plus the testbench's arithmetic: a ceiling, not an
event-driven framework like simjig and cocotb. Their loops reproduce the event-driven
order exactly (the hashes prove it): at time 0 one eval with the initial inputs (simjig's
kernel evaluates before applying the clock's first edge, so the first sample sees `empty`
high), then per cycle: the monitor samples the pre-edge values, `clk = 1; eval()`, the
producer and consumer write their inputs, `clk = 0; eval()`. The three "tasks" of C are
three steps of one loop body, called in the order the schedulers run them. Within that:
- Marlin's `eval()` copies every input field into the model (one FFI call per input, five
  here), calls Verilator's `eval` (one), and copies every output back (four): ten
  indirect calls into a `dlopen`ed `.so` per eval, two evals per cycle. A read is a field
  read, so the monitor's reads of the inputs (`push`, `pop`, `wr_data`) read back what the
  testbench wrote. Marlin keeps no simulated time.
- The C++ loop reads and writes the model's port members directly and advances
  `VerilatedContext` time by 5 ns per half period (nothing reads it: no delays).
- The reference model is a `VecDeque` (Marlin) or a `std::deque` (C++), and failed
  checks are `assert_eq!` (Marlin) or a compare that prints and exits 1 (C++).

**Timing.** Each testbench times its own body (from the first statement to the end of
the last task), so model construction, Python start-up, cocotb's regression setup and
the harness's report are excluded; `process s` is the wall time of the whole process
(simjig and Marlin: the test binary running one test, which for Marlin includes its
`verilator --version` check and the `dlopen`; cocotb: the runner's simulator process;
C++: the executable), for reference. Builds are never timed. Each point is 5 runs, each in a fresh process, run one
at a time; the table gives the median, min and max of the in-testbench time and
cycles/s = N / median, ns/cycle = median / N. Speedup vs cocotb = cocotb median / the
flow's median; time vs cpp = the flow's median / the C++ median (how far from the
ceiling).

**Idle machine.** Before every batch (one flow, variant and size: 5 runs), the driver
waits until the 1-minute load average is below `--max-load` (2.5: the idle floor here was
about 1.9, see Results) and no `cargo`, `rustc`, `clang`, `verilator` or `nextest`
process is running; the load it saw is in each result line and the table's `load`
column. Nothing is built while measuring: `bench/run.sh build` comes first.

**Correctness check.** `bench/run.sh bug` deletes `|| do_pop` from the push condition
(a full FIFO then drops a push that comes with a pop) and runs C for 10 000 cycles in
every flow. All four fail at the same check (`results/bug.txt`): `count` is 15 where the
model holds 16 (cycle 496).

## Caveats

- Marlin and C++ are hand-written loops (see Method): they show what the model and the
  arithmetic cost, with nothing scheduled. simjig's and cocotb's numbers include a
  scheduler that the loops do not have: edges found by watching signals, tasks woken in
  order, writes applied at defined points, delta cycles. A loop gets the same results
  here only because its author wrote the cycle's order out by hand; with several clocks,
  combinational feedback through the testbench or many independent tasks that order is
  the testbench author's job, and the loops' advantage is partly that work not done.

- The FIFO is tiny, so its model's eval is cheap and its numbers are dominated by the
  testbench and the scheduler; `mid` (see Designs) is the counterweight, where every
  flow spends about the same 1.3 us per cycle in Verilator's eval. It is still synthetic:
  a single clock, one long pipeline, no X propagation, no memories beyond the FIFO's.
- The stimulus RNG is a hand-written xorshift32, not `ctx.rng` or Python's `random`: the
  price of identical stimulus. In C it costs cocotb three Python method calls per cycle (two draws by the
  producer, one by the consumer).
- cocotb's assertions are rewritten by pytest (installed in the env, cocotb's default
  `COCOTB_REWRITE_ASSERTION_FILES=*.py`): 4 asserts per cycle in C, a little slower than
  plain `assert`. simjig's `assert_eq!` formats only on failure.
- C's monitor reads 7 signals, B's 6. simjig's harness also records a trace hash of
  every scheduler event (always on); that is part of what is measured.
- Apple M6, macOS; other machines and Linux will differ. The load floor of about 1.9
  (see Results) leaves many idle cores, but shares caches and memory bandwidth.
