# Notes: spikes

A spike here is an experiment kept as a regression test. These are small, real
experiments on Verilator 5.052 (pixi) and the designs in `tests/duts`, run before fork
checkpoints, DPI-C and waves were built. The code in `tests/spikes` is reference material,
not production code. Each claim below names the test that checks it.

## How to run
```sh
pixi run spikes                 # cargo nextest run --release in tests/spikes: 27 tests
pixi run spikes --no-capture    # also prints the SPIKE measurement lines quoted below
```
- `tests/spikes` is a standalone Cargo workspace (own `[workspace]`), so it is not part
  of `cargo xtask ci`. Its outputs go to `target/spikes`.
- `build.rs` verilates nine models with `--threads 1` and compiles everything with the
  pinned compiler. Each model's generated C++ and its shim go into `libspk_<prefix>.a`;
  libverilated plus `shim/runtime.cpp` are compiled **once** into `libspk_verilated.a`.
  It uses neither make nor `verilator --build`.
- Every model has a shim (`shim/model.inc`) that exports one `spk_vtable_<prefix>()`, a C
  table of function pointers. This is the shape of DECISION.md's `simjig_vtable_<prefix>`.
- Timings below were measured on an Apple M6 (12 cores, macOS, 16 KiB pages) with a
  release build. They are single runs: read them as orders of magnitude.

## Summary
| # | Experiment | Result | Most important rule |
|---|---|---|---|
| 1 | `fork()` checkpoints | **PASS**, with conditions | Call `contextp->threads(1)`; fork only when the test process has no other active threads; never touch an inherited trace |
| 2 | Several models in one process | **PASS**, with conditions | Switch `Verilated::threadContextp()` before every eval; give each model a private `$urandom` state |
| 3 | DPI into Rust | **PASS** | Catch the panic in the Rust function, park it, re-raise it after `eval()`, and poison the model |
| 4 | `--timing` loop | **PASS** | `next = min(wheel, nextTimeSlot())`, asked only when `eventsPending()`; never step past a design slot |
| 5 | Derived-clock visibility | **PASS** (a flop-derived clock is never missed); an intra-eval pulse is never visible | Compare watched signals after **every** eval |

## 1. `fork()` checkpoints (`tests/fork.rs`)
`in_child` (`src/fork.rs`) forks the process, runs a closure in the child, and returns
the child's report over a socket. The child ends with `_exit`.

| Check | Test | Result |
|---|---|---|
| Three children from one checkpoint (counter at 10): reset+7 → 7, hold → 10, 300 more cycles → 54. The parent keeps count 10 and time 210, and its continuation matches a run that never forked | `child_diverges_and_parent_keeps_its_state` | pass |
| Threads under nextest: parent 2 (libtest main thread + test thread), child 1 | `only_the_forking_thread_survives` | pass |
| A panic in the child: exit 101, parent unaffected | `panic_in_child_is_contained` | pass |
| Default `VerilatedContext` + a `--threads 1` model: 2 → **13 threads**. With `threads(1)`: no extra threads | `default_context_starts_a_thread_pool_even_for_threads_1_models` | pass |
| 64 MiB model memory (`fifo` at `DEPTH=2**23`, `WIDTH=64`): CoW timing, and the parent's memory unchanged after the child writes to every page | `fork_and_copy_on_write_cost_with_a_64_mib_model` | pass |
| FST open at fork (four tests, below) | `fst_*` | pass |

**Measurements**
- `fork()` latency seen by the parent: 0.36 ms (64 MiB model). It stays about the same
  with 256 MiB (0.36 ms) and 1 GiB (0.39 ms) of touched ballast. On macOS the cost does
  not grow with RSS. Linux copies page tables at fork, so expect it to grow there (not
  measured).
- Copy-on-write: dirtying all 64 MiB in the child took 5.0–7.3 ms (16384 writes, one per
  4 KiB; 444 ns per write ≈ 1.8 µs per 16 KiB page). The same writes on private pages took
  0.07 ms. Rewriting the pages in the child after the copies took 0.4–1.4 ms. A branch
  pays CoW only for the pages it writes. A testbench that touches little memory pays
  almost nothing, and one that rewrites a large memory pays about 0.1 ms per MiB.

**Surprises**
1. **`--threads 1` does not make the process single-threaded.** `VerilatedContext`
   defaults `m_threads` to the process's hardware parallelism (`verilated.h:582`). The
   first model added to it creates a `VlThreadPool` of `m_threads - 1` workers
   (`addModel` → `threadPoolp()`, `verilated.cpp:3455`). On this machine that is 11
   threads per context. With `contextp->threads(1)` before the first model, no pool is
   created. A forked child still evaluated a `--threads 1` model correctly with the pool
   gone (it never dispatches to it), but the pool object then refers to dead threads.
   Verilator provides `model->prepareClone()` / `atClone()` for this; they were not needed
   with `threads(1)`.
2. **Forking from a multi-threaded test process deadlocks.** With plain `cargo test`
   (libtest runs tests concurrently on threads), `fst_child_can_detach_and_trace_to_its_own_file`
   hung for good. The child's stack ended in `fst::Writer::setDate` → `localtime` →
   `_pthread_rwlock_lock_wait`: another test thread held libc's `localtime` lock at fork
   time. Under `cargo test -- --test-threads=1` and under nextest all tests pass.
3. **An FST open at fork time:** with `threads(1)` the FST writer adds no threads
   (2 threads with a trace open). The writer is fstcpp, a `std::ofstream` that writes
   value-change blocks on flush/close and seeks back to offset 0 on close to write the
   header.
   - The child never touches the trace and `_exit`s: the parent's file is valid
     (`fst_child_that_ignores_the_trace_leaves_the_parent_file_valid`).
   - The child dumps and closes the inherited trace: **the parent's file is corrupt**
     (`bad block type 0 len 29681 at offset 659 of 896`). The child and parent share the
     descriptor and its file offset (`fst_child_that_closes_the_inherited_trace_corrupts_the_parent_file`).
   - The child redirects the inherited descriptor to `/dev/null` (`dup2`), closes the
     inherited writer, and `open()`s a new file: both files are valid. The child's file
     ends at t = 710, 50 cycles after the fork (`fst_child_can_detach_and_trace_to_its_own_file`).
   - The checker is a structural walk of the FST block chain (header first, every block
     type valid, chain ends exactly at EOF, header `end_time`). It does not decompress
     anything.
4. The pixi environment had no `lz4.h`/`zlib.h`, so Verilator's FST writer did not
   compile. `pixi.toml` now pins `lz4-c` and `zlib`; waves need them too.

**Rules for `simjig::fork`**
1. Unix only. Build every model with `--threads 1`, **and** construct every
   `VerilatedContext` with `threads(1)` before adding a model.
2. Fork only when no other thread can hold a lock: run under nextest (one test per
   process; the only other thread is libtest's main thread, blocked waiting for the test).
   Before forking, check the thread count (`task_threads` / `/proc/self/task`) and return
   an error if it is above 2. Never fork under multi-threaded `cargo test`.
3. The child is the branch and the parent is the checkpoint. The child must never return
   into the test harness (its main thread is gone). It catches panics, reports over a
   pipe/socket, and ends with `_exit`, never `exit` (which would run atexit handlers and
   flush stdio buffers copied from the parent).
4. At fork time, flush every trace. In the child, either never touch an inherited trace
   (no dump, flush, close or destructor), or detach first: `dup2` `/dev/null` over the
   inherited descriptor, then `close()` the writer and `open()` a new file. Simpler
   alternative, matching DECISION.md ("waves from a deterministic replay"): branches run
   untraced, and a failing branch is replayed from the start with tracing.
5. `Drop` does not run in the child (because of `_exit`). Do not rely on it there for
   anything except memory.

## 2. Several models in one process (`tests/multi_model.rs`)
The models are `Vrng_a` and `Vrng_b` (the same design, `rtl/rng_disp.sv`, under two
prefixes; no shared DUT uses `$urandom` or `$display`), plus `Vcounter` and `Vfatal` (two
different designs). Each has its own `VerilatedContext`, and all run on one thread.

| Check | Test | Result |
|---|---|---|
| Same design under two prefixes, and a different design, alternating on one thread | `same_design_twice_and_two_designs_alternate_on_one_thread` | pass |
| `$display` and `$time` without a switch: A's output went to **B's** buffer and printed B's time (2000, not 1000). With `threadContextp` switched before each eval, each context gets its own | `display_and_time_follow_thread_context_not_the_models_context` | pass |
| `$finish` in `Vfatal` without a switch sets `gotFinish` on the **counter's** context; with the switch, on its own | `finish_sets_the_thread_contexts_flag` | pass |
| `$urandom`: A's values while interleaved with B are A-alone values 0, 2, 4, 6... (one stream dealt out alternately). With a private `VlRNG` swapped in around each eval, A is independent of B | `urandom_stream_is_per_thread_unless_swapped_per_model` | pass |
| Creating a `VerilatedContext` mid-run restarts every model's `$urandom` stream; the private swap is immune | `creating_a_context_mid_run_reseeds_the_thread_stream` | pass |
| `Vrng_a`/`Vrng_b`: 74 strong symbols each, none in common, none shared with libverilated. `Vcounter` built twice with the same prefix: 88 of 88 collide | `prefixed_models_share_no_strong_symbols_with_each_other_or_libverilated` | pass |
| Forced whole-archive link: distinct prefixes + one libverilated links; the same prefix twice fails (`duplicate symbol 'Vcounter::nextTimeSlot()'`) | `whole_archive_link_detects_a_duplicate_prefix_but_accepts_distinct_ones` | pass |
| The first eval after construction never sees an edge (it records the previous values) | `first_eval_never_sees_an_edge` | pass |

**Mechanism** (5.052 headers; confirmed by the tests above):
- `$time` is `VL_TIME_Q()` = `Verilated::threadContextp()->time()` (`verilated_funcs.h:394`).
  It does not use the model's own context.
- `$finish`, `$stop` and `$fatal` (`vl_finish`, `vl_stop`, `vl_fatal`) set flags on
  `Verilated::threadContextp()`.
- `$display` → `VL_WRITEF_NX` → `VL_PRINTF_MT` → `VL_PRINTF` (default `printf`). No
  context is involved. To route output per model, compile **every** translation unit with
  `-DVL_PRINTF=<hook>` (headers use the macro too) and key the output on
  `threadContextp()`. The spikes do this with `spk_printf`.
- `$urandom` is `VL_RANDOM_I()` → `VlRNG::vl_thread_rng()`, a `thread_local` state. It is
  reseeded from `threadContextp()`'s seed whenever the global seed epoch changes, and the
  epoch changes whenever any context's seed is set, including at context construction.
  `verilated.cpp:541`: "A thread always belongs to a single context".
- `Verilated::threadContextp(ctx)` is one `thread_local` store. Constructing a model sets
  it to that model's context. Nothing else switches it, and `eval()` does not.

**Rules for simjig**
1. Before every `eval()` (and every `final()` and trace call) of a model, call
   `Verilated::threadContextp(model's context)`. Put this in the shim's eval wrapper, not
   in Rust, so it cannot be forgotten.
2. Give each model a private `VlRNG` and `std::swap` it with `VlRNG::vl_thread_rng()`
   before and after each eval (`spk_rng_swap`). Seed it from the simjig per-component
   seed (DECISION.md table 2, row 3). Without this, `$urandom` results depend on the order
   in which models are evaluated and on when contexts are created, which breaks
   determinism. `VlRNG` has a public copy and `VlRNG(uint64_t)`; this uses only public API.
3. Route output with `-DVL_PRINTF=simjig_printf` in libverilated and in every model TU.
4. Distinct `--prefix`es are enough for models to share one binary. libverilated must
   exist exactly once: build it once per (Verilator version, defines), as DECISION.md
   table 2, row 10 says. The `VM_*` defines affect only the `WAVES` macro and coverage in
   5.052, so one build with the union of features (timing, FST, DPI) serves every model.
   Compile everything with the same `-std` (gnu++20 here, needed for `--timing`).
5. With static archives a duplicate prefix does not fail the link: the linker takes the
   first definition it finds. simjig-build must reject duplicate prefixes itself.
6. Settle every model with one eval at its initial input values before the first clock
   edge. The first eval only initialises the trigger state.
7. **DPI note:** a DPI *import* is one global C symbol, so every model in a binary that
   imports `dpi_add` gets the same implementation. Two designs that need different
   functions under one name cannot share a binary. DPI *exports* were not tested. So a
   parameter variant does not need its own test binary: distinct prefixes share a binary fine; only DPI name clashes force separate binaries.

## 3. DPI into Rust (`tests/dpi.rs`)
`dpi_add` and `dpi_tick` are `#[unsafe(no_mangle)] pub extern "C"` functions in the test
binary; `dpi_impl.c` is not linked. They are defined in the integration-test crate (not
the library), so the linker always sees them before the model archive.

| Check | Test | Result |
|---|---|---|
| `sum = dpi_add(2, 3) = 5` from inside eval; 5 posedges → 5 `dpi_tick` calls | `rust_dpi_functions_run_inside_eval` | pass |
| A panic inside the guard is caught, parked in a `thread_local`, re-raised after `eval()` and caught by the test. The model kept going with the dummy return value (`sum = 0`) | `panic_in_dpi_is_caught_on_the_rust_side` | pass |
| A panic outside the guard in an `extern "C"` fn: the process aborts (SIGABRT, signal 6) | `uncaught_panic_in_an_extern_c_dpi_function_aborts` | pass |

**Measurements** (best of 5 runs of 20M calls through a volatile function pointer from
C++): C++→C++ 0.53 ns, C++→Rust plain 0.42–0.50 ns, C++→Rust with `catch_unwind` + two
`thread_local` accesses 1.5 ns. A whole `eval()` that calls `dpi_add` once: 15–20 ns. The
guard costs about 1 ns per call; `catch_unwind` itself is free on the happy path, and the
cost is the thread-local accesses.

**Rules for simjig**
1. Every Rust DPI function body runs under `catch_unwind`. On a panic it stores the
   payload and returns a neutral value.
2. After every `eval()`, the kernel checks the parked payload. If one is there, it
   **poisons the model**, because the design has already consumed a wrong return value,
   and reports a `TaskPanic`-like failure with the DPI function name. It never resumes
   evaluating that model.
3. Do not use `extern "C-unwind"` to let a panic run through Verilator frames: eval state
   (`VerilatedEvalLoop`, triggers) would be half-updated. Not tested.
4. Generate the DPI declarations from `V<prefix>__Dpi.h` so the signatures cannot drift.
   Here they were written by hand.

## 4. `--timing` loop (`tests/timing.rs`)
The loop (`merged_loop`) evals at t = 0, then repeats: `design_next = eventsPending() ?
nextTimeSlot() : none`; `next = min(design_next, tb_next)`; set the time; toggle `start` if
the testbench edge is due; one eval. The testbench half period is 7 ns, so it interleaves
with the design's 5 ns slots and coincides with them at 35 and 70 ns.

| Check | Test | Result |
|---|---|---|
| 34 evals to 100 ns (22 design slots, 14 testbench edges, 2 shared). Time strictly increases. `ticks = 5` at 50 ns; `done` falls at 7 ns and rises at 30 ns. `iclk` changes only at design slots | `merged_loop_keeps_time_monotonic_and_matches_the_readme` | pass |
| `nextTimeSlot()` with `eventsPending()` false: **the process exits with code 1**, both for a model without `--timing` ("No delays in the design") and for a `--timing` model before its first eval ("There is no next time slot scheduled") | `next_time_slot_exits_the_process_when_no_event_is_pending` | pass |
| Setting time past a pending slot (5 ns + 1 ps) and evaluating: **exit 1** ("Encountered process that should've been resumed at an earlier simulation time. Missed a time slot?"). Going back in time (5 ns → 2 ns) is not checked at all | `skipping_past_a_design_slot_is_fatal_but_going_backwards_is_not_checked` | pass |

**Rules for simjig**
1. Call `nextTimeSlot()` only when `eventsPending()` is true. Otherwise Verilator calls
   `vl_fatal` → `std::exit(1)`, which kills the test process with no Rust failure.
   `eventsPending()` is also false after `$finish`.
2. The next time is `min(wheel, nextTimeSlot())`. Advancing past a design slot is fatal in
   the same way, so the testbench must never set a time beyond `nextTimeSlot()`.
3. The kernel itself must assert that time never goes backwards; Verilator does not.
4. `nextTimeSlot()` can return the current time (pending `#0` processes,
   `VlDelayScheduler::nextTimeSlot`). The loop must then eval again at the same time, so
   counting this as a delta and not a new time step is the kernel's job. Not exercised
   here, because timing_dut has no `#0`.
5. Coincident testbench edges and design slots share one eval (2 of 34 here). That is
   consistent with DECISION.md §1.
6. Install `VL_USER_FATAL` (or route `vl_fatal`) so these misuses become `Failure`s and not
   `exit(1)`. The fatal-routing spike (DECISION.md §4 Q1) should cover `vl_fatal` from
   `verilated_timing.cpp` as well as from generated code.

## 5. Derived-clock visibility (`tests/derived_clk.rs`)
`Vderived` is `derived_clk` with `rtl/derived_clk.vlt` (`public_flat_rd` on `div_clk`).
`div_clk` is also a top port, so the `.vlt` adds a second member,
`derived_clk__DOT__div_clk`. `Vglitch` is `rtl/glitch.sv`: an internal clock that is set
by `posedge clk` and asynchronously cleared by itself, so it is high only between delta
iterations of one eval.

| Check | Test | Result |
|---|---|---|
| Sampling after every eval over 10,000 clk cycles: design 5000 `div_clk` rising edges, observed 5000, **0 missed**. The port and its public copy never differ | `derived_clock_sampled_after_every_eval_is_never_missed` | pass |
| Sampling every k-th eval (k = 1..4): observed 5000, 5000, 1667, **0** of 5000 | `derived_clock_sampled_less_often_than_every_eval_aliases` | pass |
| Intra-eval pulse: the design counted 1000 pulses (232 mod 256), the testbench saw `pulse = 1` **0 times** in 2000 evals | `pulse_inside_one_eval_is_invisible_to_sampling` | pass |

**Quantified:**
- A flop-derived clock changes at most once per eval, so sampling after every eval
  misses 0%.
- Sampling less often aliases. Every 2nd eval sees all edges here only because 2 evals =
  1 clk cycle. Every 3rd eval misses 67%, and every 4th misses 100% (the sample lands on
  the same `div_clk` phase each time).
- A pulse that starts and ends inside one eval is missed 100% of the time, with or
  without `public_flat_rd`.

**Rules for simjig**
1. The kernel compares watched signals after **every** eval (each delta), never once per
   time step. Otherwise a design-driven clock that changes twice in one time step (two
   clk pulses written in deltas) aliases as above.
2. A trigger on an internal derived clock is "sampled" in a precise sense: it fires once
   per eval in which the post-eval value differs from the previous post-eval value. An
   edge that occurs and reverts within one eval (a self-clearing pulse, a delta glitch)
   is never reported. Document this next to DECISION.md §1 ("Triggers on internal
   derived clocks are documented as sampled") and in `tests/conformance` vocabulary.
3. `public_flat_rd` on a top-level output is redundant (the port already exists). It
   creates a second member that tracked the port exactly here. For internal signals it
   is required, so read the `__DOT__` member.

## Contradictions and refinements of the design documents
- DECISION.md §4 Q3 asked whether `fork()` works "single thread, `--threads 1`, no open
  trace files". It works, but `--threads 1` is not enough: the context also needs
  `threads(1)` (§1 surprise 1). Open traces are survivable only with the detach procedure.
- A common rule is "each HDL-parameter variant gets its own `--prefix` and its own test
  binary". The prefix is required; a separate binary is not. Only DPI name
  clashes force separate binaries (§2 rule 7).
- A common rule is to "compile the shim so fatals do not exit". `nextTimeSlot()` misuse and
  a skipped time slot reach `vl_fatal` → `exit(1)` from libverilated, not from the shim. A
  `VL_USER_FATAL` hook must be compiled into libverilated itself.
- `pixi.toml` lacked FST's `lz4`/`zlib` (fixed in `c43da94`).

## Not verified
- Linux (all numbers and runs are osx-arm64). Fork latency there should grow with RSS.
  GNU `nm`/`--whole-archive` paths in the collision tests have never run.
- `prepareClone()`/`atClone()` with a real thread pool (`--threads > 1`).
- DPI exports, `svGetScope` inside a Rust context function, and `extern "C-unwind"`
  through Verilator frames.
- `#0` (zero-delay) handling in the `--timing` loop.
- The FST checker does not decompress value-change data. "Valid" means structurally
  sound with the right header end time.
- Two different Verilator versions in one binary (not supported; one libverilated).
