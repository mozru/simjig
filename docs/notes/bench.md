# Notes: simjig vs cocotb benchmark

The method, the commands and the full table are in `bench/README.md`. This note covers
where the time goes and what is worth fixing in simjig.

## Where the time goes (1M cycles, per cycle, medians)
| | simjig | cocotb |
|---|---:|---:|
| A: clock, scheduler, model eval (2 timesteps) | 623 ns | 1332 ns |
| B - A: one task wake plus 6 signal reads | 25 ns | 2211 ns |
| C - A: reset, 3 tasks, 3 writes, 7 reads, model check | 400 ns | 6638 ns |

- **cocotb.** The GPI clock keeps A cheap: each cycle is one C++ clock toggle plus one
  Python wake. B adds one more task wake and six `int(handle.value)` reads (VPI, then
  a `Logic`/`LogicArray`): 2.2 us. I did not split that further. In C, everything beyond
  A (the Python testbench and its wakes) is 83% of the time per cycle.
- **simjig.** The testbench costs almost nothing (B - A is 25 ns), so the scheduler's
  fixed cost per timestep is about 60% of C. A is only 2.1x faster than cocotb's C++ clock
  plus one Python wake: simjig's kernel overhead per timestep is the place to win.
- The model is not the difference: Verilator's `evalImpl` is 7% of simjig's C, and
  `--public-flat-rw` (the `fifo_pub` rows) costs simjig about 1%.

## Profile of simjig's full testbench (C)
Taken with macOS `sample` (8 s at 1 kHz, on the idle machine, release build with line
tables) of `c_full::fifo` at 20M cycles; 6074 samples on the test thread. Inclusive shares:

| Share | Where |
|---:|---|
| 54% | `sched::apply_and_compare` (apply the write log, eval, compare watched signals) |
| 25% | `tasks::run_ready` (polling the three tasks, testbench code included) |
| 21% | `Models::eval`, of which Verilator's own `evalImpl` is only 7%; the rest is the shim around it: `simjig_handle::guard` (exception barrier, `std::vector<Record>::reserve`, `VlDeleter::deleteAll`, mutex) and `VerilatorModel::outcome` / `Runtime::drain` (5%) |
| 20% | `WaitTable::fire_next_change` (edge waits) |
| ~15% | **the allocator**: `free` 10.4% (8.4% of it is `mach_absolute_time`, which macOS's xzone `free` calls) plus `malloc` and `RawVec::grow` |
| 8.7% | `Recorder::record`: the always-on trace hash, FNV-1a over 40 bytes a byte at a time, plus a `VecDeque` ring push and pop per record |

The clock-only variant (A) has the same shape, with the allocator at about 18%.

## Hotspots worth fixing
1. **Per-timestep heap churn (about 15% of C, 18% of A).** `free` is called from
   `sched::run_loop` and `waits::wake_sorted`. The allocations, every timestep:
   - `sched.rs` (`run_loop`, the edges due at `t`): `let edges: Vec<(u32, u32)> = ...collect()`;
   - `waits.rs`: each wait group's `keys: VecDeque<Key>` grows on its first
     `Registration::new` (`VecDeque::grow`, 4.4%), and `WaitTable::unprime` returns it by
     value, so it is freed after every fire. Groups also go in and out of the
     `groups: BTreeMap<u64, Group>` every edge (`remove_kv` 4.3%, `insert_recursing`).

   Reusing scratch vectors kept in the kernel (as `scratch`/`spare` already do elsewhere
   in `WaitTable`) and recycling group key buffers should remove most of it. The order of
   wakes must not change: the conformance suite (`tests/conformance`) pins it.
2. **The recorder (8.7%).** Hashing five `u64`s per record a byte at a time; a
   word-at-a-time hash would be cheaper, but the trace hash is a stable format
   (`kernel::recorder::Fnv`, a single home in ARCHITECTURE.md), so changing it changes
   every recorded hash: a semantics change that needs its own decision.
3. **The eval shim (about 11% beyond Verilator's eval).** `guard()` reserves a
   `std::vector<Record>` and runs `VlDeleter::deleteAll` on every eval; both could be
   skipped when nothing was queued.

Not verified: the size of any gain. These are profile readings, not experiments; none
of them was changed in this branch.

## Surprises
- cocotb's runner cannot run a test from a fresh `Runner` (`_vhdl_sources` missing)
  unless `hdl_toplevel_lang` is given; `bench/cocotb/run.py` passes it.
- cocotb's `verilated.mk` compiles at `-Os`; conda's `CXXFLAGS` add `-O2` in front, but
  `OPT_FAST` comes last and wins, so `MAKEFLAGS` sets all three `OPT_*` to `-O2`.
- A stray `find /` of mine (from setup) was left running during the measurement. It used
  1.5 s of CPU in two hours (blocked on I/O) and was killed before profiling; it is not
  in any number to a measurable degree, but it was there.

# Marlin and raw C++ flows

Two more flows, so simjig can be placed between cocotb and the ceiling: Marlin 0.21.1
(`bench/marlin/`) and a raw C++ Verilator `main` (`bench/cpp/main.cpp`). Method,
caveats and the final measurements are in `bench/README.md`.

## Marlin
- "Merlin" in the request is Marlin (crates.io `marlin`, github.com/ethanuppal/marlin,
  "No-nonsense hardware testing in Rust"). 0.21.1, released 2026-10-02, is the latest
  release at least two weeks old (0.21.1 is also the newest). MPL-2.0.
- The API: `#[verilog(src, name)] struct Fifo;` generates a struct with one public field
  per port; `VerilatorRuntime::new2(artifacts, sources, includes, dpi, options)` and
  `runtime.create_model_simple::<Fifo>()` verilate the sources (at run time, on first use;
  cached by source mtime in the artifacts directory), link a `.so` and `dlopen` it.
  `eval()` (trait `AsDynamicVerilatedModel`) pins every input, evals and reads every
  output back, each through an `extern "C"` function pointer. `marlin-test` (a separate
  crate, not used) adds a `Seq` wrapper with `tick()`; the core API is the loop.
- No parameter overrides: the macro reads port widths from the top module's source
  (`$clog2(DEPTH)` included, evaluated with the defaults), and `VerilatedModelConfig` has
  no `-G`. Hence `bench/marlin/rtl/sync_fifo_bench.sv`, a wrapper with literal widths.
- No extra Verilator or compiler flags either (`VerilatedModelConfig` has the `-O` level,
  warnings to ignore, the C++ standard, include and library paths, and the compiler).
  The C++ gets `-O2` through `MAKEFLAGS` in the environment, which the `make` that
  Verilator's `--build` runs reads (checked with `make -n -B`: `OPT_FAST` is `-O2`).
  Marlin cannot pass `--no-timing`; with no delays in the design that changes nothing.
- It works with the pixi Verilator 5.052 (Marlin requires 5.025 or later).

## Surprises
- A `//` comment whose first word is "Verilator" is a Verilator pragma
  (`BADVLTPRAGMA`); the wrapper's comment had to be reworded.
- The first B hash from both loops was wrong: before any `eval()`, Verilator's outputs
  are zero (`empty` low), but simjig's kernel evaluates once at time 0 before applying the
  clock's first edge, so its first sample sees `empty` high. Both loops now start with
  one eval; then every hash matched at 10k and 1M.
- Marlin's build banner says "`verilator-O0` profile [unoptimized]" when no `-O` is
  passed: that is Verilator's default optimisation, not `-O0`.

## Smoke checks (10k cycles, load 4.4; not measurements)
- `bench/run.sh build`: all four built (58 s).
- `bench/run.sh run --sizes 10000 --runs 1 --max-load 100 --out target/smoke.jsonl`,
  then `table`: no mismatch. Every flow reported A hash 0, B hash 3228338176, C pushes
  4160, pops 4155, hash 1744841553 (the committed simjig/cocotb values). At 1M (Marlin and
  C++ only, by hand): C 402910/402901/3888034673, as committed.
- `bench/run.sh bug`: all four fail at `count`, 15 against the model's 16 (C++: cycle
  496).
- Orders of magnitude only (loaded machine): Marlin and C++ both run C at about 35 to
  40 ns/cycle, about 25x less than simjig's 1 us.

## A full run
`BENCH_RUN_ARGS="--sizes 1000000,10000000 --runs 5 --flows simjig,cocotb,marlin,cpp" bench/run.sh`
runs env, build, bug, the 120 measured runs and the table, and rewrites
`bench/results/`. Estimated from the committed process times: simjig 130 s, cocotb
720 s, Marlin and C++ about 10 s each; with build and bug check about 17 minutes, plus
any wait for the load gate (< 2.5). The committed final runs (`results/final-*.jsonl`)
add the simjig-elide and simjig-sync flows.

## The mid-size design
Why: on the FIFO, Verilator's eval is 7% of simjig's time, so every ratio is a framework
ratio. `bench/rtl/mid_pipe.sv` (`--design mid`; `bench/README.md`, "Designs") puts a real
model cost under the same push/pop testbench.

- **Why synthetic, not PicoRV32 or SERV.** No download, no third-party license, and the
  testbench interface stays the FIFO's: a CPU needs a memory model served by the
  testbench, which in cocotb is a Python callback per access and would turn the
  comparison back into a testbench-cost one. The price: it is not a real core (one
  clock, one long pipeline, no control-heavy logic).
- **Why stages in inverse pairs.** The checker must be cheap in Python, or cocotb's
  number measures a reference model. With each odd stage undoing the even one (odd
  multiplier and its inverse mod 2^64, computed in the RTL by a Newton-iteration constant
  function), `out_data` is the popped entry: the scoreboard is a queue. Verilator does
  no algebra across registers, so the work is not optimized away; the CRC lane, which
  hashes every intermediate value, makes sure each stage's arithmetic is observed (it
  is in every flow's result hash).
- **Sizing.** Smoke runs of the C++ flow (load 6 to 8, so rough): 32 stages about
  0.5 us per cycle, 64 about 0.7 us, 128 about 1.3 us. 128 it is: in the middle of the
  0.5 to 5 us aimed at, about 12 400 flops. Verilating and compiling it takes seconds.
- **Smoke run** (10 000 cycles, 1 run, load about 6: not a measurement), ns per cycle,
  C (full): simjig 2 367, simjig `mid_pub` 2 758, Marlin 1 320, C++ 1 318, cocotb 11 639.
  Every flow reported the same pushes, pops and hash at every variant.
- **Bug check.** All four flows fail `c_full` on the injected bug (`out_valid`); the FIFO
  design's bug check still fails all four too (`count`, cycle 496).
- **Not done.** No profile of where simjig's extra time per cycle goes on `mid`.

Refactor on the way: each flow's helpers that both designs use (xorshift32, drivers, run
length, result line, the C++ `Bench` stepper) moved to one shared file per flow
(`simjig/tests/common/`, `marlin/tests/common/`, `cocotb/bench_common.py`,
`cpp/common.h`); `cargo fmt` reformatted the two Rust `bench.rs` files it touched.
