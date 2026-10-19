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

# Phase 2: storage pointers, fewer lookups, an opt-in mode without the trace hash

Same method. ns per cycle, each row measured against the row above; instructions per
simulated cycle of C in brackets.

| Commit | A | B | C | C instr. |
|---|---:|---:|---:|---:|
| end of phase 1 | 200.2 | 225.1 | 408.9 | 14051 |
| `kernel::cell`: the narrow storage pointer moves to the kernel (refactor) | | | | |
| read and write narrow slots through the model's storage pointer | 197.9 | 218.7 | 395.3 | 13701 |
| release a fired registration in the poll that sees it fire | 196.3 | 217.9 | 392.0 | 13636 |
| queue our own tasks without calling their wakers | 196.7 | 218.3 | 391.2 | 13530 |
| skip a walk after evals alone when only inputs are watched | 196.8 | 217.3 | 388.7 | 13209 |
| find the current kernel through one plain thread-local | 191.1 | 209.4 | 372.6 | 12656 |
| the recorder's state in `Cell`s | 187.3 | 204.8 | 365.0 | 12367 |
| leave a C++ `Scope` without a second `$urandom` epoch sync | 189.9 | 205.9 | 367.6 | 12205 |
| **`trace_hash = false`** (opt-in) | 156.0 | 173.1 | 308.7 | 10816 |

Main at bd73904 against the end of phase 2, one session: A 621.8 -> 188.2 (3.3x), B 644.4
-> 205.6 (3.1x), C 1019.5 -> 368.3 (2.8x); with `trace_hash = false` A 156.0 (4.0x), B
173.1 (3.7x), C 308.7 (3.3x). The ceiling (a hand-written Verilator loop, Marlin) is 35
to 40 ns for C: simjig is 9x off by default, 8x without the hash.

## What phase 2 did with the director's list
1. **Pointer-based signal access: done for the last hop.** A Verilator backend hands the
   kernel each readable narrow slot's storage pointer (`kernel::cell::Direct`, made from a
   validated signal-table row; width and writability checked when the table is decoded).
   `Models` reads and writes through it: typed `get`, every value-change comparison and
   snapshot, and the write log's application. Writes still go through the log and are
   applied at the same points. The unsafe surface: `kernel::cell` (the two raw accessors,
   moved there from `model::abi`, which re-exports them, and `Direct`), two blocks in
   `kernel::models`; Miri runs `kernel::cell`'s test over every storage class; every
   Verilator-model test, the goldens and the C++ UBSan spike exercise it. Not done:
   handles carrying the pointer themselves, with no kernel lookup at all. A handle is
   `Copy` and `Send`; reading model memory through it without the kernel would need a
   liveness and thread check of its own, which costs about what the lookup now costs
   (the next point).
2. **Lazy eval: not done; it conflicts with the semantics.** The only eval that could be
   saved is one of the two at the falling edge (the edge's own, and the one that applies
   the writes made at it). Evaluating them as one changes what the design sees: logic
   triggered by the edge (`always @(negedge clk) q <= push`) would sample the new input
   instead of the old one, which is what cocotb shows and the goldens pin. Deferring the
   write's eval to the next timestep instead does not save an eval (it still runs, later)
   and runs the design code the write triggers at the wrong `$time` (displays, DPI-C calls,
   assertions). The skipped evals of phase 1 (nothing written since the last eval) are
   no-ops for a model without a variable C++ may write; they stay recorded and counted.
   (Corrected after review: "provably no-ops" was wrong for a model with a
   `public_flat_rw` variable, whose logic runs on every eval; see "Corrections" below.)
3. **A fast path for quiet timesteps: done** as far as it is provable (phase 1: empty
   ReadWrite round, empty regions, skipped walks; phase 2: a walk after evals alone when
   only inputs are watched). (Corrected after review: the phase 2 skip assumed Verilator
   rejects an assignment to an input, but `ASSIGNIN` is a lint a design may waive, and
   then an eval changes an input; the skip was removed. See "Corrections" below.)
4. **Wakers and polling: done where it did not change behaviour.** Wakers were already
   static (no allocation). The current kernel is now found through one plain thread-local
   holding its address (`kernel::state::with_entry`, one unsafe block) instead of a
   `RefCell<Option<Rc>>` and a second thread-local for the DPI-C flag; a fired trigger's
   registration is released in the poll that sees it; a wake of one of our tasks queues it
   directly. Not done: removing the lookup when a trigger is made (it validates the handle
   at the call; deferring that moves a misuse panic from `rising()` to its first poll).
5. **The opt-in mode: `#[simjig::test(trace_hash = false)]`.** It drops exactly one
   guarantee: the run keeps no trace hash. Events are still recorded, counted and kept in
   the recent-events window; the outcome, the report and `summary.json` carry a null hash,
   so two runs cannot be compared by it, and a waves replay checks only that it ended the
   same way, not that it took the same path. 16% faster on C. The default is unchanged.

## The floor, and what limits it
Instructions per cycle of C now: 12.2k by default, 10.8k without the hash; the machine
retires them at an IPC of about 7, so time follows instructions, except that the trace
hash adds a serial chain of dependent multiplications (one per byte of every event) that
the no-hash mode removes. No single part dominates any more; a single-stepped cycle of C:

| Part | Instructions |
|---|---:|
| Verilator's own `eval`, 3 times (its loop, `deleteAll` with its mutex, the message queue flush, `eval_ico`/`act`/`nba`) | about 1400 |
| The shim around each eval (C++ `guard`/`Scope`, `$urandom` slot, Rust `outcome`, the message count) | about 900 |
| Trace records, 17 (default mode; the hash is about 60 of each 100) | about 1700 |
| Value-change walks, 5 (fire-and-wake, the walk, firing the waiters) | about 1000 |
| Awaits, 3 (trigger made, registered, fired, woken, polled, released) | about 2400 |
| Signal access: 7 reads and 3 writes through the typed handles and the log | about 1300 |
| The scheduler loop (regions, clock edges, next time) | about 800 |
| The testbench itself | about 400 |

Three of these are fixed by the semantics: 3 evals (not 2) per cycle, the region
sequence, and one walk per application of the log. The rest is the price of a general
executor (any number of tasks, waits of every kind, combinators, cancellation, misuse
checks) paid on every await and access. The raw loop retires about 1300 instructions per
cycle; reaching 2x of it would need everything but Verilator's own 1400 to fit in about
1500 instructions, a quarter of what this design spends. I see no way to that by trimming:
it needs a different executor for the common case (for example, tasks that only await
edges of kernel clocks compiled into a per-edge schedule with the waits resolved once,
signals read through pointers held by the schedule, and evals merged where the design is
known to have no logic on the edge), which changes the executor's semantics and needs its
own design decision (rule 5) and review cycles.

## Also measured, not done
- **A faster hash of the time word** (the largest part of each record's chain): FNV-1a
  over a fixed word is `h * P^n + C(h & 0xff)`, so a per-timestep table of the time
  word's low-byte chain (NEON can build it in about 100 cycles) would shorten each record's
  chain from about 45 to about 20 cycles while keeping every value. Estimated 15 to 25 ns
  on C in default mode; arch-specific code for less than the no-hash mode gives.
- **Handles holding storage pointers** (no kernel lookup per access): would save about 40
  instructions per access (400 per cycle of C), but needs a liveness and thread check per
  access to stay sound, which is most of the saving.

# Phase 3: synchronous tests, and the async executor again

## The table (1M cycles, 5 interleaved runs, median ns per cycle)
Measured in one session, idle-gated; the C++ loop and Marlin are bench2's binaries
(`bench/cpp/main.cpp`, `bench/marlin`), run on this machine with the same gate. Every flow
reported the same pushes, pops and hashes (C: 402910, 402901, 3888034673).

| Flow | A | B | C | C instr./cycle |
|---|---:|---:|---:|---:|
| C++ Verilator loop (the ceiling) | 27.8 | 28.1 | 35.5 | 1073 |
| Marlin | 33.7 | 33.4 | 39.7 | |
| **simjig sync** (`#[simjig::test(sync)]`, bound handles) | 33.2 | 34.5 | 43.0 | 1382 |
| simjig async, `trace_hash = false` | 135.8 | 147.5 | 274.6 | 9629 |
| simjig async (default) | 150.3 | 161.7 | 304.8 | 10726 |
| main at bd73904 (async) | 622.6 | 652.2 | 1026.2 | 24250 |

Sync is within 1.2x of the C++ loop (A 1.19x, B 1.23x, C 1.21x) and within 1.1x of
Marlin on C. Async is 3.4x faster than at the start (C), and 8.6x the ceiling.

## Steps (each measured against the one before, in its commit message)
| Commit | sync C | async C |
|---|---:|---:|
| sync tests: `#[simjig::test(sync)]`, `Sim::run_sync`, `SyncClock` | 102.8 | 368 |
| shim: `hold`/`release`/`eval_held` (C ABI, appended) | 86.4 | |
| one `DirectTable` for `Models` and the kernel's handles | 76.1 | 374 |
| `SyncClock::bind`: bound handles | 55.7 | |
| model C++ as one translation unit (`VM_PARALLEL_BUILDS = 0`) | 52.7 | 358.9 |
| no panic barrier around a held model's eval | 51.7 | |
| frame pointers as the platform has them | 47.8 | 364.4 |
| a bound handle keeps its `Direct` | 45.7 | |
| the clock calls the held eval directly (`HeldEval`) | 41.5 | |
| hold a lone model for an async run too | | 320.5 |
| a timestep's fixed record tails hashed with one lookup | | 303.7 |

## What a sync cycle costs now (C, 1382 instructions)
Verilator's own two evals are about 910 (the C++ loop's whole cycle is 1073, its evals
about 950: simjig's model build now matches `verilated.mk`'s). The rest: the testbench
body with its ten bound accesses (about 300, against about 100 in C++, where an access is
one load and the value conversions are free), each edge's clock write, time and checks
(about 75), and the call into the held eval with its C++ barrier (about 45 per eval). What
limits it: the value conversions of the typed API (`Bit`, `Bits<N>` from raw words), the
phase and failure check on each edge (a stopped or ended run must not evaluate), and the
exception barrier around each eval (`try`, the outcome flags). Each is a few instructions;
none is removable without giving up a check.

## What I did not do, and why
- **Lazy eval in async** (merge an edge's eval with the writes made at it): changes what
  edge-triggered logic samples (phase 2 notes). Not done.
- **A compiled per-edge schedule for async.** From the breakdown (C, 10726 instructions):
  three evals are fixed by the semantics (about 1400 of Verilator's), the trace records
  about 1500 (hashed by default), and the scheduling, walks, wakes and accesses the rest.
  A per-edge schedule that keeps the semantics (three evals, every record, the walk after
  each application of the log, the region sequence) would still spend about 1400 + 1000 +
  1000 = 3400 instructions per cycle: about 100 ns, 3x the ceiling, not 2x. Within 2x
  needs fewer evals (a semantic change) and no hash; sync mode is that design. Not built.
- **Edge-wait fast path, cheaper walks, cached handle pointers in async**: each is a few
  percent of the async cycle (the profile is flat: no function above 15%), and the larger
  of them change the wait table's structure under the wake-order guarantees. Measured
  candidates that gained nothing were dropped (a prefix table for `Write` records: 302.4
  -> 300.2 ns, within noise).
- **A per-timestep table for the time word of the trace hash** (NEON can build the
  low-byte chain for all 256 states in about 100 cycles): would shorten each record's
  serial chain further; the whole hash now costs 30 ns of async C (304.8 against 274.6
  without it), so this would recover perhaps 15 ns, with architecture-specific code.

# Corrections (review of phases 1 to 3)

Two of the "provable" skips were not, and are now gone where they cannot be proven.

- **Eval elision** (phase 1: an eval with nothing written since the last one is skipped).
  Verilator 5.052 schedules logic fed by a variable C++ may write (`VLVF_PUB_RW`:
  `public_flat_rw`, `public` or `public_flat` on a variable, `--public-flat-rw`,
  `--public-depth`, `--public-params`, a read-write probe) under the 'ico' region's
  'first iteration' trigger (`V3Sched.cpp`), which every `eval()` sets. That logic runs on
  every eval, written or not, so skipping one changed what it saw (`$time`) and did
  (`$display`, DPI-C calls); tests `model_tests::elision` (a probe, and a metacomment with
  nothing written). simjig-build now reads the scope table after verilating
  (`syms::writable_public`) and compiles such a model with `SIMJIG_PUBLIC_RW=1`, which
  the shim reports as `SIMJIG_FEATURE_PUBLIC_RW`; the backend then never settles. Checked
  on 5.052's output: `--public` alone and `--vpi` alone make no variable writable and put
  nothing under that trigger, so they keep elision.
- **The walk after evals alone when only inputs are watched** (phase 2). It assumed
  Verilator rejects an assignment to an input; `ASSIGNIN` is a lint a design may waive,
  and then an eval changes an input and a waiter on it woke half a period late
  (`model_tests::walks`). Nothing outside the design shows such a write (`--json-only`
  marks the written submodule port, not the top's input), so the skip is removed: the
  Verilator backend keeps `eval_changes` true for every slot. The kernel mechanism
  (`WaitTable::eval_groups`, the clause in `sched::fire_value_changes`) is left in place,
  now unused by any shipped backend, to keep this change out of `kernel/sched.rs` while
  the scheduler is rebuilt on `sched`.
- **Not fixed, a known gap.** The same 'first iteration' trigger also runs logic fed by an
  input the design writes (`ASSIGNIN` waived) and by a `$sampled` variable. For pure
  combinational logic that is a no-op, so elision is still right; for such logic that
  reads `$time`, displays or calls DPI-C, a skipped eval differs from a C++ loop. It is not
  detectable from the build's inputs (see above).

## Cost (bench C, 1M cycles, 5 interleaved rounds, idle-gated by `bench.py`'s gate)
Median ns per cycle; int-p @ 667e341 against this branch; every run reported the same
pushes, pops and hash (402910, 402901, 3888034673).

| Model | before | after | |
|---|---:|---:|---:|
| `fifo` | 302.4 | 314.0 | +3.8% |
| `fifo_pub` (`--public-flat-rw`) | 305.2 | 346.8 | +13.6% |
| `fifo` sync (`c_full_sync`) | 42.0 | 42.4 | noise |

A second session with a third build (this branch with the walk skip put back) attributes
all of `fifo`'s cost to the walk: 303.2 before, 314.3 after, 302.5 with the skip. So
`fifo_pub` pays about 11 ns for the walk and 30 ns for evaluating every eval, as a C++
loop does. Correctness over performance (constitution); a provable form of the walk skip
would need Verilator to say which inputs a design writes.

# Eval elision off by default (int-q)

## Decision
`p1-fix` left a known gap: Verilator 5.052 re-runs, on every `eval()`, the logic under the
'ico' region's 'first iteration' trigger, which includes logic fed by an input the design
writes itself (`ASSIGNIN` waived) and by a `$sampled` value. If that logic is impure
(`$time`/`$realtime`, `$display`/`$write`, `$random`/`$urandom`, a DPI-C import), a skipped
eval differs from a C++ loop. Correctness first, so elision is now **off by default** and
an explicit opt-in: `#[simjig::test(elide_evals)]`, `ModelOptions::with_elide_evals(true)`
(its rustdoc states when it is safe). The opt-in still never elides for a model the build
shows is unsafe (`SIMJIG_FEATURE_PUBLIC_RW`, a writable non-input row, `--timing`, waves).

The alternative, proving a design pure from `--json-only` or the scope table and keeping
elision on for exactly those designs, was not built: the proof needs "writes no input",
which neither output shows (`--json-only` marks the written submodule port, not the top's
input; see "Corrections" above), so it could not be made conservative. Tests:
`model_tests::elision` (`input_time.sv`: by default `t_out` follows `$time` with nothing
written; opted in it stays stale, the documented limit; opted in on `rw_meta` it still
evaluates every time).

## Cost (bench C on `fifo`, 1M cycles, 5 interleaved rounds, idle-gated)
Gate: 1-minute load < 2.5 and no cargo, rustc, clang, verilator or nextest process before
each run (loads 2.34 to 2.46). int-q after the merges of `p1-fix` and `sched`. Both
reported the same pushes, pops and hash (402910, 402901, 3888034673).

| Flow | median ns/cycle | runs |
|---|---:|---|
| `simjig` (elision off, the default) | 298.2 | 296.1, 296.5, 298.2, 299.4, 327.5 |
| `simjig-elide` (`elide_evals`) | 272.4 | 272.1, 272.1, 272.4, 272.5, 272.9 |

The default costs 25.8 ns per cycle (9.5%): the evals that open each timestep and close
each delta loop now run (and the walks after them find nothing). The bench keeps both as
separately labelled flows (`bench.py --flows simjig,simjig_elide`).
