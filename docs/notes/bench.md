# Notes: simjig vs cocotb benchmark (agent "bench")

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
