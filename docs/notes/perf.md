# Notes: simulation throughput (agent "perf")

Goal: simjig's throughput on the benchmark's `sync_fifo` testbench (`bench/README.md`),
with semantics bit-identical: the conformance goldens, the wake order, every kernel test,
replay and fork, and the trace hash. The profile this started from is
`docs/notes/bench.md`.

## Method
- **Timing.** The bench binary (`bench/simjig`, `cargo test --profile bench`), model `fifo`,
  variants A (clock only), B (monitor) and C (full), 1M cycles. Builds are compared
  *interleaved*: for each variant, 5 rounds, each running every build once, each round
  only when the 1-minute load is below 2.5 and no cargo, rustc, clang, verilator or
  nextest process runs (the gate of `bench/bench.py`). The table gives the median
  in-testbench time per cycle. Every run of every build reported the same pushes, pops and
  hashes (C: 402910, 402901, 3888034673; B: 1594171392).
- **Instructions.** `/usr/bin/time -l` reports instructions retired and CPU cycles; the
  difference between a 2M-cycle and a 1k-cycle run, halved, is per simulated cycle, with
  no noise. That is the number to decide by when a change is below the timing noise.
- **Where they go.** `sample` for inclusive shares; for instruction counts per function,
  inline path and source line, an `lldb` script that single-steps one simulated cycle
  (two timesteps) after 2000 warm-up timesteps.
- **Breakdowns** come from throwaway builds with one part removed (never committed): no
  trace record (`Recorder::record` returns at once), no shim (`VerilatorModel::eval`
  returns after the raw call), no eval at all (A only, as C then fails its checks), no
  C++ `Scope`.

## Before and after
ns per cycle, median of 5 interleaved runs; each row was measured against the row above
in one session, so the "before" of a row can differ from the row above by the noise
(about 1%).

| Commit | A | B | C | C cycles/s |
|---|---:|---:|---:|---:|
| main @ bd73904 | 622.8 | 652.0 | 1025.1 | 0.98M |
| reuse the scheduler's per-timestep buffers | 590.7 | 616.3 | 980.9 | 1.02M |
| keep value-change groups and their buffers | 558.5 | 582.9 | 927.6 | 1.08M |
| recorder: zero top bytes in one step, a fixed ring | 423.7 | 452.3 | 728.9 | 1.37M |
| skip an eval that has nothing to do | 346.1 | 376.5 | 646.0 | 1.55M |
| drop stale group entries at the front | 324.4 | 357.6 | 600.0 | 1.67M |
| skip a walk when no model changed | 314.1 | 343.2 | 582.7 | 1.72M |
| ask whether an eval left messages | 304.6 | 332.1 | 573.0 | 1.75M |
| recorder: an all-ones word with one lookup | 253.8 | 282.2 | 496.7 | 2.01M |
| narrow group snapshots as one array | 248.3 | 277.3 | 485.5 | 2.06M |
| bench profile: codegen-units = 1, thin LTO | 229.8 | 256.2 | 444.9 | 2.25M |
| skip an empty ReadWrite round and log | 209.4 | 234.8 | 420.3 | 2.38M |
| skip empty NextTimeStep and ReadOnly | 205.3 | 230.2 | 416.9 | 2.40M |
| a flag for "the run failed" | 199.6 | 226.1 | 410.3 | 2.44M |
| walk order in a Vec with a moving head | 199.2 | 225.1 | 405.5 | 2.47M |

First against last, in one session: A 622.7 -> 198.1 (3.1x), B 646.2 -> 224.0 (2.9x),
C 1018.4 -> 405.5 (2.5x). The bench profile change is about 10% of that; the rest is the
code. Instructions per simulated cycle: A 14527 -> 7178, B 15682 -> 8109, C 24250 -> 14054;
CPU cycles per simulated cycle: A 2963 -> 943, C 4864 -> 1904 (IPC 5.0 -> 7.4).

Allocations per steady-state cycle (`tests/alloc_steady.rs`, the Verilator `counter`):
clock only 4 -> 0, monitor and driver 6 -> 0.

## Where a cycle of C goes now (405 ns, 14054 instructions)
From the single-stepped cycle and the removal builds:

| Part | Instructions | ns (removal build) |
|---|---:|---:|
| Trace records (17 per cycle, about 100 instructions each) | 1772 | 54 |
| Value-change walks (`fire_next_change` and its fire-and-wake) | 1327 | |
| Three real evals: Verilator's `eval` (its loop, `VlDeleter::deleteAll` and its mutex, `endOfEval`) | about 1400 | |
| The shim around them: C++ `guard`/`Scope` (thread context, `$urandom` state swap), Rust `outcome`/`drain` | about 1000 | about 8 per eval |
| Thread-local lookups (`_tlv_get_addr`: every signal access, trigger and registration) | 792 | |
| Task polls, wakes, registrations, triggers (`run_ready`, `Registration::{new,drop}`, `Edge::poll`) | about 2800 | |
| Signal reads (7) and writes (3) through the typed handles and the write log | about 2000 | |
| The rest of the scheduler loop (`step`, `next_time`) | about 1000 | |

A (198 ns, 7178 instructions): the records are 1100 instructions and 28 ns; the two evals
with their shim about 55 ns; the kernel the rest.

## Against the 40 ns ceiling
A hand-written Verilator loop and Marlin do the work of C in 35 to 40 ns per cycle, with
2 evals. simjig is now at 405 ns: 10x, not within 2x. I do not see a way to parity with
the semantics fixed; these are the parts that cannot shrink much without changing a
guarantee, in order of size:

1. **The scheduler's structure.** Every timestep runs cocotb's region sequence (the timed
   region, deltas with a value-change walk after each eval and after each application of
   the log, ReadWrite, ReadOnly), and every task wake is a poll with its own registration,
   waker and trace record. A raw loop does none of this. Each step of it is now a few
   tens of instructions, but there are hundreds. Reaching parity would need a different
   execution model for the common case (for example a compiled schedule for tasks that
   only await a clock edge), which is an executor-semantics change.
2. **Three evals per cycle in C, not two.** The writes made at the falling edge are
   applied after that edge's eval and need an eval of their own (cocotb does the same:
   `tests/conformance`). A raw loop sets the inputs and the clock together. Merging them
   changes when a write takes effect.
3. **The always-on trace hash.** 17 records per cycle of C are one serial FNV chain (each
   byte is a dependent multiply). The removal build saves 54 ns of C (28 of A): an upper
   bound on what any faster hash could gain. A word-at-a-time or SIMD hash would get most
   of that but changes every recorded hash (seeds, failure signatures and replays compare
   them), so it is a decision for the director, not done here. Making the hash opt-out
   (a fast mode) saves the same 54 ns and drops the determinism check of DECISION §2 row 4
   for such runs.
4. **Signal access through the current kernel.** A typed `get` is about 100 instructions:
   the thread-local current kernel (two thread-locals on macOS, each a `_tlv_get_addr`
   call), the handle's simulation and width checks, the `RefCell`, two dynamic calls and
   the signal-table row. Reading Verilator's storage through a pointer fetched at create
   would make it a load, but needs a new `unsafe` surface reaching from handles into model
   memory, against the invariant that no Rust reference to model memory exists. Not done.

So with every guarantee kept, the floor of this design is perhaps 250 to 300 ns for C
(each of the remaining parts trimmed further, not removed). Below that needs one of the
changes above.

## What I did not change, and why
- **The C++ `Scope`** (thread context and the `$urandom` state swap on every entry):
  removing it entirely saves about 10 ns per cycle of A (5 per eval). Keeping the model's
  state resident in the thread between evals would get most of that, but changes which
  state non-simjig Verilator code on the same thread sees between evals, the isolation
  docs/notes/tracecov.md relies on. Not worth the risk for 5 ns.
- **The mutex** is not simjig's: it is `VlDeleter::deleteAll` and the message queue
  flush inside the `eval()` Verilator generates (`evalBegin`/`evalEnd`). Skipping them
  means calling Verilator internals (`VerilatedEvalLoop`), which 5.052 does not expose.
  The shim's own path (`guard`, `Runtime::drain`, `outcome`) has no lock.
- **`std::vector<Record>::reserve`** (in the original profile) was `Runtime::drain`
  reserving 0 on every eval; drain now returns before touching its buffers when nothing
  is pending, and the Rust side asks first.
- **One thread-local for the kernel and the DPI-C flag** (so an entry point looks up one):
  same instruction count, no measurable change; reverted.
- **`-O3` and `-mcpu=native`** for the model: no measurable change on this design.
- **Fat LTO**: no gain over thin.
- **Timers**: a deadline's key buffer is now reused, but the wheel is still a `BTreeMap`,
  which frees its root node when it empties, so a testbench whose only wait is a `Timer`
  allocates one node per timestep. The allocation test covers the clock and edge waits
  only.

## Surprises
- Allocation was a smaller share than the original profile suggested (macOS's `free`
  calls `mach_absolute_time`, which inflated it); the trace hash was a larger one, because
  the whole run's records form one dependency chain.
- An all-ones word (every record without a task) is a fixed byte string, and FNV-1a over
  a fixed string maps `h` to `h * P^len + C[h & 0xff]`: only the low byte carries from one
  byte to the next. That turned 8 dependent steps into one lookup, value for value.
- Without `codegen-units = 1`, the kernel's hot path stays split across codegen units and
  does not inline; that is a tenth of the time.
- The bench's committed results (`bench/README.md`, `bench/results/`) predate all of this
  and the profile change; they were not rerun (that needs cocotb too).
