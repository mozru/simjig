# tracecov: FST tracing, coverage and per-model isolation at the C level

Agent `tracecov`, branch `w3/tracecov`. Checked against the installed Verilator 5.052
(`.pixi/envs/default/share/verilator/include`) and the 5.053-devel source.

## How to run
```sh
pixi run cpp-tests      # crates/simjig-build/cpp/tests/run.sh: 18 C++ tests, about 1 min
```
`run.sh` verilates five models (`Vtc_full` with `--trace-fst --coverage`, `Vtc_plain`,
`Vtc_hier` with `--coverage-line`, `Vtc_timing` with `--timing --trace-fst`, `Vtc_cg` with a
covergroup and `--coverage-line`) into one
executable, the same way `cpp/spike/run.sh` does. `traits.cpp` writes their traits the way
the shim emitter does. The tests use only the vtable. `cargo xtask ci`
runs it (since s1c2fix).

## The ABI after this change (`SIMJIG_ABI_VERSION` stays 1)
`simjig_model_vtable` grows from 136 to **192 bytes**, append-only. The first 136 bytes keep
their layout. The old trace/coverage slots are now filled.

| Offset | Field | C type | Meaning |
|---|---|---|---|
| 104 | `trace_open` | `simjig_status (*)(h, const char* path)` | `trace_open_ex(h, path, 0, 0)` |
| 112 | `trace_dump` | `simjig_status (*)(h, uint64_t time)` | one sample; OK no-op if no trace or `time < start_time` |
| 120 | `trace_close` | `simjig_status (*)(h)` | close; OK no-op if none open |
| 128 | `cov_counters` | `uint32_t* (*)(h, size_t* count)` | live pointer, or NULL/0 (below) |
| 136 | `features` | `uint64_t` | `SIMJIG_FEATURE_TIMING 1`, `_TRACE_FST 2`, `_COVERAGE 4` |
| 144 | `trace_open_ex` | `simjig_status (*)(h, const char* path, uint32_t depth, uint64_t start_time)` | open (closing an open one first) |
| 152 | `trace_flush` | `simjig_status (*)(h)` | flush buffered samples |
| 160 | `trace_reopen` | `simjig_status (*)(h, const char* path)` | fork child: detach and reopen |
| 168 | `cov_zero` | `simjig_status (*)(h)` | zero every counter |
| 176 | `cov_write` | `simjig_status (*)(h, const char* path)` | write `coverage.dat`; NULL = Verilator's default name |
| 184 | `rng_seed` | `simjig_status (*)(h, uint64_t seed)` | private `$urandom` state = `VlRNG(seed)` |

New status: **`SIMJIG_IO_ERROR 7`**: a file the call names cannot be created, or
`trace_reopen` was given the inherited file itself. A WARNING message names the file. Nothing
ran, and the model is not poisoned. Without the pre-check, `VerilatedCovContext::write` reports
an unwritable file with `VL_FATAL_MT`, which would poison a healthy model. fstcpp's
`std::ofstream` reports nothing at all, so the trace would silently be empty.

Trace and coverage entries return `SIMJIG_UNSUPPORTED` unless the model was built with
them. They map the sticky FINISH and STOP to OK: those describe the simulation, not the
call. FATAL, EXCEPTION and POISONED pass through.

The Rust mirror (`crates/simjig/src/model/abi.rs`) must append the seven fields above and
`Status::IoError = 7`, and change the size assert to 192. Until it does,
`abi_counter::rust_mirror_matches_cpp_layout` fails (C reports 192, Rust 136). I applied that
patch temporarily and all 69 `simjig` tests passed, then reverted it, because `crates/simjig`
belongs to another agent.

### ABI field table (`simjig_rt_abi_fields`)
`const simjig_abi_field* simjig_rt_abi_fields(size_t* count)` (in `simjig_rt.h`, defined in
`simjig_rt.cpp`) returns a static array of
`{const char* type; const char* name; uint32_t offset; uint32_t size;}` (24 bytes, align 8).
There is one entry per field of `simjig_model_vtable` (25), `simjig_signal` (9), `simjig_msg`
(7) and `simjig_create_opts` (6), **47 in all**. The structs come in that order, and the
fields of each in declaration order. `name` is the C field name; the Rust mirror renames only
`final` to `final_`. A Rust test can look up each `(type, name)` and compare it with
`offset_of!`/`size_of` of the mirror field, and check that the count per type equals the
mirror's field count. The C++ test `abi_field_table_covers_every_field` checks that, within
each struct, offsets start at 0, never overlap and leave only tail padding.

## Tracing
- `trace_open_ex` creates a `VerilatedFstC` and calls `model.trace(tfp, 0)` and then
  `tfp->open(path)`.
  - The traits must set `kTraceFst`. `simjig_model.h` then calls
    `ctx.traceEverOn(true)` before constructing the model: generated `trace_init` fatals
    otherwise ("requires Verilated::traceEverOn(true) call before time 0"). Only traced
    builds pay for it, because it makes Verilator compute signals that only the waves read.
  - `static_assert(kTraceFst == Model::traceCapable)`: a shim that disagrees with
    the Verilator command line does not compile. Tested in
    `trace_flag_that_disagrees_with_the_model_fails_the_build`.
- **Depth.** 5.052's generated `traceBaseModel` ignores `levels` (`(void)levels`). So depth uses
  `tfp->dumpvars(depth, model.name())` before `open`, with `$dumpvars` semantics:
  - depth 1 traces the top scope's own signals (5 for `tc`);
  - 0 traces everything.
- **Trace from time t.** Dumps with `time < start_time` return OK without touching the
  writer. Verilator's first real dump is a full dump, so the sample at the first time `>= t`
  is complete. The FST header's start time is that first dumped time. The test checks the
  header and every value-change block's start time. A trace that is open but not dumped
  costs only Verilator's per-eval activity flags. The kernel still has to call
  `trace_dump`; a no-op call is a branch.
- **Fork: detach and reopen** (spikes.md §1 surprise 3). At open, the trace file's
  `(st_dev, st_ino)` is recorded. `trace_reopen(path)` in the child:
  1. refuses `path` if it names the same inode (`IO_ERROR`; opening it would truncate the
     parent's waves);
  2. `dup2`s `/dev/null` over every descriptor on that inode;
  3. destroys the inherited writer, whose header and blocks go to `/dev/null`;
  4. opens `path` with the same depth and start time.

  The parent should `trace_flush` before `fork()`.

  The test checks that the parent's file is **byte-identical** before and after the child
  ran, and that both files parse. A structural check alone was not enough: without the
  detach, the parent's later writes reuse the same offsets and paper over what the child
  wrote (found by mutation, see below).
- `destroy` closes an open trace before the model is destroyed (`ModelHandle::teardown`). In a
  fork child, `destroy` of a handle with an inherited, non-detached trace would corrupt the
  parent's file. Children `_exit` and never destroy (spikes.md §1 rule 5).
- Link: traced models need `verilated_fst_c.cpp` plus `-llz4 -lz`, and coverage models need
  `verilated_cov.cpp` (and `verilated_covergroup.cpp` per `_classes.mk`). Models without them
  need neither: all trace and coverage code sits in `if constexpr` branches of templates, so
  `crates/simjig`'s build (no FST, no coverage) still links. **For buildinfra:** add those
  sources to libverilated when any model in a binary uses them.

## Coverage: how counters map to points
Verilator 5.052 keeps coverage counters in **several** arrays, not one:
- each module *object* has `uint32_t __Vcoverage[local bins]`: the top's root object, plus
  one per instance of a module kept un-inlined (`no_inline_module`, hierarchical blocks);
- the symbol table has `__Vcoverage[all bins]`, which only static/package/class functions
  increment.

`_configure_coverage` registers each bin with `VL_COVER_INSERT(countp, keys...)`:
- once with the object-local counter;
- once with the symbol-table counter (or a shared fake zero for later instances).

Both use the same keys, and `coverage.dat` sums items with equal keys.

`cov_counters` returns `rootp->__Vcoverage` **only when its length equals the symbol table's**.
That means every point of the model is declared in the top object. Otherwise it returns
NULL/0 and does not hand out a partial array: an index would then name different points in
different builds. `cov_zero` (`VerilatedCovContext::zero`) and `cov_write` always cover every
point.

Mapping, for a flat model (`tc`, `-O` defaults inline everything; `--flatten` forces it):
- index `i` is Verilator's local bin number `i` in the top object. It is assigned at
  verilation, so it is stable for one build, not across Verilator versions or design edits;
- a line/branch/user point is one counter;
- a toggle point of a `W`-bit signal at base `b` uses counters `b + 2k` (`[k]:1->0`) and
  `b + 2k + 1` (`[k]:0->1`) (`VL_COV_TOGGLE_CHG_ST_I`, `__vlCoverToggleInsert`);
- **one counter can be several points.** Verilator registers aliased signals with the same
  counter: in `tc`, `tc_sub.clk` uses `tc.clk`'s counters and `tc_sub.q` uses `tc.odd`'s. So
  `coverage.dat` has more points than counters (100 vs 96 here), and its total count is
  higher (299 vs 269). The only stable name for a point is its `coverage.dat` key (file,
  line, column, hier, page, comment). A per-index name table would need a hook on
  `VL_COVER_INSERT`. That is possible (pre-include `verilated_cov.h` from
  `simjig_vl_config.h` and redefine the macro) but not done.

Each handle has its own context and so its own `VerilatedCovContext`: `coverage.dat` is per
model.

## Covergroup bins and `cov_runs` (s1c3fix)
`cov_runs` (`docs/notes/s1c2fix.md`) hands out the counter arrays that lie inside the
`V<prefix>__Syms` object. A covergroup's bins are not there: 5.052 registers them through
`VL_COVER_INSERT` when the covergroup is constructed, which for `cg inst = new;` is the model's
first eval (static initialisers run in `_eval_static`), into counters it allocates on the
heap. Decision: **document, not expose.** The ABI comment of `cov_runs` now says the runs hold
every *code* coverage counter (line, branch, toggle, expression, FSM, user) exactly once and
no covergroup bin; `cov_write` and `cov_zero` include the bins. Why not expose them: they
appear after create, one allocation per covergroup instance, so a run list fixed at create
cannot hold them, and a list that grows during evals would hand out pointers whose layout
changes with the simulation.

The noting itself now stops when create calls `Runtime::cov_runs` (`m_cov_closed`). Before,
every registration during an eval (each covergroup `new`) was appended to a vector that was
never read again, for the model's lifetime.

Tests: `cov_noting_stops_after_create` (a registration after `cov_runs` is not kept; fails
without the flag) and `covergroup_bins_are_written_but_not_in_runs` (`Vtc_cg`: the runs do not
change across evals, `coverage.dat` has the covergroup points and a higher total than the
runs, `cov_zero` clears them).

## Per-model isolation
- **Thread context**: every `Scope` (every entry point that runs model code, trace and
  coverage included) sets `Verilated::threadContextp` to the model's context. On exit it
  restores the enclosing model's context if scopes nest. `destroy` restores an enclosing
  model's context after `~VerilatedContext` nulls it.
- **`threads(1)`** is set in `Runtime`'s constructor, before the model is added. Test:
  the process has 1 thread after two models are created and evaluated.
- **`$urandom`** (5.052 `verilated.cpp`, `VlRNG::vl_thread_rng`):
  - `$urandom` draws from `VlProcess::currentRng()`, which is the process RNG inside a
    `--timing` process and otherwise the `thread_local` `t_rng`;
  - `t_rng` is reseeded from `threadContextp()`'s seed whenever the global seed epoch moved
    since this thread last looked. Any `VerilatedContext::randSeed`, `$urandom(seed)` or
    `commandArgs(+verilator+seed+)` call moves the epoch.

  Each `Runtime` owns a `VlRNG`, which `Scope` swaps with `t_rng`. The swap goes through
  `simjig::thread_rng()`:
  - it caches `&t_rng`;
  - on every later call it saves the state, calls `vl_thread_rng()` (syncing the epoch,
    which may reseed) and restores the state.

  So a reseed meant for "the thread" never lands on a model's private state. That case arises
  when a context is created while a model's state is swapped in, e.g. a DPI call that creates
  a model.
  - Initial state: `VlRNG(context_seed())` after `commandArgs`, with the same formula as
    `randSeedDefault64` (`(s << 32) ^ s`, `s` = `+verilator+seed+`, default 1). One model
    alone sees Verilator's usual stream.
  - `rng_seed(h, seed)` replaces it with `VlRNG(seed)`, for simjig's per-component seed.
  - `$urandom(seed)` still works per model: it reseeds the slot that holds that model's
    state.
- **Exact limitations:**
  - Process RNGs (`--timing` processes, IEEE 1800 §18.14) and class objects take their seed
    from the current stream when they are constructed. `rng_seed` after create does not reach
    objects that already exist.
  - Verilator draws a nondeterministic seed for `randSeed() == 0`. `+verilator+seed+0`
    replaces 0 at parse time, so 0 only comes from code; simjig keeps it as the deterministic
    seed 0.
  - x-initialisation randomness (`VL_SCOPED_RAND_RESET_*`) uses the context's `randSeed` and
    scope hashes, never `t_rng`, so it is per-context already and `rng_seed` does not affect
    it.
- **`$display`** goes into the active runtime's ring (`VL_PRINTF` hook), with `$time` and
  `%m` from that model's context and name. Test: A (`A.tc ... t=45`) and B (`B.tc ...
  t=1045`) each see only their own line.

## Probe width check (Stage 1 review)
Before this change, `SIMJIG_SIGNAL` checked only the storage class, so a 3-bit probe
declared as 5 bits built cleanly (both are `CData`). The exact width exists only at runtime:
it is in the `VlVarTableEntry` tables of `V<prefix>__Syms__Slow.cpp`, which `public_flat_rd`/
`_rw` creates even without `--vpi`. No `constexpr` metadata exists. So `create` runs
`simjig::check_probe` on every `SIMJIG_DIR_INTERNAL` signal after `bind`:
- look up scope `<model name>.<path minus its last component>` and var `<last component>`;
- check that the var exists, that `datap()` equals the bound pointer, that it has no
  unpacked dimensions, and that `entBits()` equals the declared width.

A mismatch throws `std::runtime_error`. `create` then returns EXCEPTION with an EXCEPTION
message such as `simjig: probe 'tc.u.r': the model's variable is 1 bits wide, but it is bound
as 5 bits`, and the handle is poisoned. Tests:
- `probe_width_is_checked_at_create` (C++);
- `probe_with_a_wrong_width_fails_create` (emitter output with a widened probe, Verilator
  build, driver).

Ports are not checked this way; `check_ports` against the header macros covers them.

## Evidence
- `pixi run cpp-tests`: 18 tests pass.
- Mutation runs: each of these source changes makes the named test fail:
  - no RNG swap → `urandom_streams_are_per_model_and_reproducible`;
  - no epoch-preserving `thread_rng` → `thread_rng_survives_a_reseed_epoch`;
  - no `threads(1)` → `contexts_start_no_threads`;
  - no `threadContextp` → `display_is_attributed_to_its_model`;
  - no `/dev/null` detach → `fork_child_detaches_and_reopens`;
  - no start-time check → `trace_from_t_has_no_sample_before_t` and the timing test;
  - no `dumpvars` → `trace_depth_limits_the_signals`.
- `pixi run bash crates/simjig-build/cpp/spike/run.sh`: unchanged results (old traits without
  `kTraceFst`/`kCoverage` still compile; vtable size 192).
- `cargo nextest run -p simjig-build`: all pass, with 4 new shim snapshots, 13 updated ones
  (two new `kTraceFst`/`kCoverage` lines each) and 3 new Verilator tests.

## Not verified / open
- Linux: everything ran on osx-arm64 only. The fd scan uses `RLIMIT_NOFILE` capped at 65536.
- ASan (it cannot run on this host; see abi.md).
- A nested scope (DPI → Rust → another model) end to end; only `thread_rng`'s contract is
  tested directly.
- `cov_counters` for non-flat models returns NULL; whether simjig-build should pass
  `--flatten` with `--coverage` is open. Probes already assume inlining: they bind
  `rootp->a__DOT__b`.
- No `CHANGELOG` exists in the repository to record the ABI addition.
