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
bench/run.sh run --flows simjig,marlin,cpp ...  # some flows (simjig, simjig_pub, simjig_elide, simjig_sync, cocotb, marlin, cpp)
bench/run.sh table bench/results/final-fifo.jsonl
bench/run.sh --design mid    # the same sequence for the mid-size design: bench/results/mid/
bench/run.sh --design mid run --sizes 10000 --runs 1 --max-load 100 --out target/smoke-mid.jsonl
```

`--design` (before or after the subcommand) picks the design: `fifo` (the default) or
`mid` (see Designs).

| File | What |
|---|---|
| `rtl/mid_pipe.sv` | the mid-size design (see Designs) |
| `simjig/` | the simjig testbenches: a standalone Cargo workspace (`build.rs`; `tests/bench.rs` for the FIFO, `tests/mid.rs` for `mid`, `tests/common/` shared); flow `simjig` (async, `#[simjig::test]`), flow `simjig-elide` (the same with eval elision opted in, `#[simjig::test(elide_evals)]`: simjig's default evaluates every time) and flow `simjig-sync` (`#[simjig::test(sync)]`, `SyncClock`, bound handles: the same three testbenches as loops) |
| `cocotb/test_bench.py`, `cocotb/test_mid.py` | the cocotb testbenches, statement for statement the same (`bench_common.py` shared) |
| `cocotb/run.py` | builds and runs them with cocotb's Verilator runner; its `DESIGNS` is each design's sources, top and parameters |
| `marlin/` | the Marlin testbenches: a standalone Cargo workspace (`tests/bench.rs`, `tests/mid.rs`; `rtl/*_bench.sv`, the parameter wrappers Marlin needs) |
| `cpp/main.cpp`, `cpp/mid.cpp` | the raw C++ testbenches (`common.h` shared) |
| `bench.py` | the driver: build, bug check, measured runs, table; its `DESIGNS` is each design's testbenches and injected bug |
| `results/` | the committed results: `env.txt`, `bug.txt` (`mid/bug.txt` for `mid`), and the final runs `final-fifo.{jsonl,md}`, `final-mid.{jsonl,md}`; a full `bench/run.sh` writes `raw.jsonl` and `table.md` next to them |

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
  catches it at the same check, `out_valid` high where no entry is due
  (`results/mid/bug.txt`; C++ reports cycle 130: the first empty pop's stale entry reaching the end).

## Results

Apple M6, 16 GiB, macOS 27.0.1; Python 3.12.15, cocotb 2.1.0, Marlin 0.21.1, Verilator
5.052, rustc 1.99.1, clang 21.1.8 (`results/env.txt`). Five runs per point, each in a
fresh process; `seconds` is the in-testbench time (see Method). Full tables:
`results/final-fifo.md` and `results/final-mid.md` (from `final-fifo.jsonl` and
`final-mid.jsonl`, one line per run), at 1M and 10M cycles, for variants A, B and C.

Variant C (`full`: reset, producer, consumer, checking monitor), median ns per cycle:

| design | cycles | simjig | simjig-elide | simjig-sync | Marlin | C++ | cocotb |
|---|---:|---:|---:|---:|---:|---:|---:|
| `fifo` | 1M | 297.8 | 274.5 | 42.3 | 39.8 | 34.6 | 8,165.8 |
| `fifo` | 10M | 301.2 | 279.3 | 43.3 | 42.7 | 34.8 | 8,052.8 |
| `mid` | 1M | 1,508.1 | 1,484.8 | 1,246.5 | 1,247.9 | 1,239.6 | 11,205.4 |
| `mid` | 10M | 1,511.1 | 1,488.7 | 1,246.1 | 1,245.9 | 1,238.4 | 11,168.9 |

- On the tiny `fifo`, async simjig is about 27x faster than cocotb and 8.7x slower than
  the C++ loop: the time is the scheduler's, not the model's. simjig-sync is within
  1.2x of C++ and level with Marlin.
- On `mid`, where Verilator's eval is about 1.24 us of every cycle in every flow, async
  simjig is 7.4x faster than cocotb and 1.2x the C++ loop; simjig-sync and Marlin are
  within 1% of C++.
- Eval elision (`simjig-elide`, opt-in) saves 7-8% on `fifo` and under 2% on `mid`.
- Throughput is flat from 1M to 10M cycles in every flow. In C, max/min of the five runs
  is at most 4% for the simjig flows, 1.3% for cocotb and 10% for Marlin; the noisiest
  point in the tables is C++ `clock_only` on `fifo` at 1M (one run 35% slow), which the median
  absorbs.
- Every flow reported the same pushes, pops and hash at every point.

Caveats, in short (see Caveats below): the C++, Marlin and simjig-sync loops have no
scheduler, so they are ceilings, not like-for-like with simjig and cocotb; both designs
are synthetic and single-clock; one machine, one OS. `load` in the tables is the highest
1-minute load average seen before a point's batches (1.07 to 2.27); every batch waited
for the idle gate (see Method), and no build ran during any batch.

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
built with `cargo test --profile bench` (release, plus line tables, one codegen unit and
thin LTO: `bench/simjig/Cargo.toml`). Marlin labels its
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

**simjig-sync, Marlin and C++: a loop, not a scheduler.** None has tasks, triggers, delta cycles or
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
- simjig-sync (`tests/bench.rs` and `tests/mid.rs`, the `*_sync` tests) is simjig's own
  loop: `#[simjig::test(sync)]`, one `SyncClock::tick()` (two evals) per cycle, and
  handles bound to the clock (`SyncClock::bind`), so a read or write is a pointer access.
  Every output of both designs is registered, so a read after `tick()` is the pre-edge
  value the async monitor sees.
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
waits until the 1-minute load average is below `--max-load` (2.5, above the measuring
machine's idle floor of about 1.9) and no `cargo`, `rustc`, `clang`, `verilator` or `nextest`
process is running; the load it saw is in each result line and the table's `load`
column. Nothing is built while measuring: `bench/run.sh build` comes first.

**Correctness check.** `bench/run.sh bug` deletes `|| do_pop` from the push condition
(a full FIFO then drops a push that comes with a pop) and runs C for 10 000 cycles in
every flow. Every flow fails at the same check (`results/bug.txt`): `count` is 15 where the model
holds 16 (cycle 496).

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
  (see Method) leaves many idle cores, but shares caches and memory bandwidth.
