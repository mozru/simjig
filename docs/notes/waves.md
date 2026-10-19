# waves: FST tracing and waves on failure

Code: `crates/simjig/src/model/{trace,verilator}.rs` (plus four `RawModel` trace calls in
`model/raw.rs`, `FEATURE_TRACE_FST` in `model/abi.rs` and two `LoadError` variants in
`model/meta.rs`), `crates/simjig/src/harness/{waves,replay}.rs` (plus `runner.rs`,
`report.rs`, `dut.rs`), `crates/cargo-simjig/src/waves.rs`. Tests: `crates/simjig/tests/waves_tests/`
(FST models `counter_fst` and `fifo_fst` added to `crates/simjig/build.rs`), unit tests in
`model::verilator` (fake vtable that records trace calls) and `harness::waves`, and
`crates/cargo-simjig/tests/it` (the CLI end to end).

## Getting waves in one command
```sh
# from a failure report's `waves:` line (works anywhere the repro works):
SIMJIG_WAVES=1 SIMJIG_SEED=0x70b4f8e1d587b46e cargo nextest run -p simjig -E 'test(=on_fail::counter_overflows)'
# or
cargo simjig waves on_fail::counter_overflows --seed 0x70b4f8e1d587b46e [-p simjig] [--window 2us..3us]
# or, always on (CI): every failure gets waves
SIMJIG_WAVES_ON_FAIL=1 cargo nextest run
```
The report then prints:
```text
  waves:    <run dir>/waves.fst (from t=2 us); open with: gtkwave <run dir>/waves.gtkw
  replay:   reproduced (same seed, same ending at the same time, same events)
```

## Tracing API (model level)
```rust
pub struct TraceOptions;                  // simjig::TraceOptions
impl TraceOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self;   // everything, whole run
    pub fn depth(self, levels: u32) -> Self;        // $dumpvars levels, 0 = all
    pub fn start(self, t: SimTime) -> Self;         // no sample before t (ticks)
    pub fn end(self, t: SimTime) -> Self;           // no sample after t; closes then
    pub fn if_supported(self) -> Self;              // a non-FST model runs untraced
    pub fn path(&self) / depth_limit / start_time / end_time;
}
impl ModelOptions { pub fn with_trace(self, TraceOptions) -> Self; pub trace: Option<TraceOptions> }
LoadError::NoTrace { prefix }                       // "built without FST tracing ... .trace_fst(true)"
LoadError::TraceOpen { prefix, path, reason }       // IO_ERROR from trace_open_ex, NUL in path
```
Use: `let mut sim = counter::Dut::instantiate(&ModelOptions::new(seed).with_trace(t))?`.
The `instantiate` of a generated `Dut` opens the trace at load, after every check.
A FakeModel ignores `trace`.

### Sampling: no kernel hook needed
The backend dumps the end of each timestep, using the existing `ModelBackend::set_time`
contract. The kernel calls `set_time(t')` exactly once at the start of each timestep,
before it applies any write of that timestep (`kernel::sched::step`: set_time, then the
timed region only logs writes, and the log is applied after the next eval). So at
`set_time(t')` the model still holds the final state of the previous timestep `t`, and
`trace_dump(t)` there is exact. The last timestep is dumped in `Drop` (before `final`
blocks; the trace closes after them). Timestamps are ticks, the context's time unit, and
the FST header's timescale is the design's precision (-12 for tests/duts).
Test: `every_timestep_is_one_sample_at_its_sim_time` (FST time table == the recorder's
`Step` times). Mutation: dumping at `now + 1` fails it and three others.

### Failures
- A failing eval (an `$error`, or `$stop`) dumps the state it reached and flushes
  (`a_failing_eval_dumps_and_flushes_at_once`).
- After FATAL/EXCEPTION the model is poisoned. Rust then makes no trace call (the model
  notes' invariant: a poisoned model never reaches C++ again), and `destroy` closes the
  trace (`ModelHandle::teardown`). **The waves end at the last completed timestep; the
  fatal's own partial timestep is not in them** (the C side refuses `trace_dump` once
  poisoned). The failure time is still marked in the `.gtkw`.
- A TaskPanic/timeout ends the run between evals; `Drop` dumps the state at that moment, so
  the waves end exactly at the failure time (`the_waves_of_a_failed_run_end_at_the_failure`).
- A trace call that fails prints one line and stops the trace, never the run.
- Not flushed periodically: a process abort (an internal Verilator fatal in a coroutine,
  a nextest kill) loses the buffered samples.

## Waves on failure (`harness::waves`, `harness::replay`)
- `SIMJIG_WAVES=1` replays after every run (pass or fail), `SIMJIG_WAVES=window:<t0>..<t1>`
  traces that span (`2us`, `1500 ns`, `0`, `_` allowed, either end optional),
  `SIMJIG_WAVES_ON_FAIL=1` replays failures only. A bad value panics with the accepted forms.
- Default window: `max(t_end / 10, 1 us)` before the failure (or the end of a passing run),
  traced to the end. At a precision coarser than 1 us, 10% alone.
- **The replay is a child process**: `current_exe() --exact <test> --include-ignored
  --test-threads 1` with `SIMJIG_SEED`, `SIMJIG_RUNS_DIR=<run dir parent>/.<seed>.replay-<pid>`,
  `SIMJIG_TRACE=<that>/waves.fst`, `SIMJIG_TRACE_FROM/TO` (ticks), and the waves variables
  removed. Why not in-process: the test body is `FnOnce` (the macro's contract), models
  keep process state, and a fresh process is exactly what the repro line does.
  `SIMJIG_TRACE*` are read by every run (`RunEnv::trace`, optional waves) and are a usable
  low-level knob.
- The parent reads the child's `summary.json` (status, failure kind/signature/time ticks,
  end time, trace hash) and compares. **Non-determinism** = a different failure kind or
  signature, a different failure time, pass vs fail, a different end time, or (with the
  same ending) a different kernel trace hash. It prints `replay: NON-DETERMINISTIC` with one
  line per difference and a help line; it never retries. If the child wrote no summary
  (crash), the verdict is `NOT VERIFIED` with the child's last stderr lines.
- Files in the run dir (one FST per traced model and fork branch): every FST the replay
  wrote, moved from the replay's root by `replay::move_waves`:
  - `waves.fst` for the first model, `waves.model-<i>.fst` for model `i > 0` of a
    multi-DUT test (`harness::dut`, `ModelOptions::for_model`);
  - `waves[.model-<i>].branch-<j>.fst` for each `fork` branch, one `.branch-<j>`
    (`model::trace::branch_path`; checkpoints cannot nest, `ForkError::Nested`).
    `replay::trace_identity` parses the names; any other file is left where it is;
  - one `<stem>.gtkw` per FST (`[dumpfile]`, `[timestart] <from>`, the primary marker at
    the failure time in ticks, a zoom fitting the window into ~1000 px, the failing
    signals as `TOP.<kernel name>` for WriteConflict/WriteInReadOnly/DeltaLimit);
  - `replay/` (the replay's `summary.json`, `events.jsonl`, to diff against this run's).

  `WavesReplay::files: Vec<WavesFile>` (each with `fst`, `gtkw`, `model`, `branch`,
  `marker`, `failing_branch`) lists them; it replaced the single `fst`/`gtkw` fields.
  Order: the files of the fork branch that failed the test first, then the rest in
  (branch, model) order. The report's `waves:` line names the first file (with its model
  or branch label when it is not model 0 of the main process) and prints one continuation
  line per other file.
- A failing fork branch: `Branches::into_values` records the first failed branch (index
  and its failure time) in the thread-local `fork::FailedBranch`; the runner takes it
  after a failed run and clears it at the start of every run. The test fails at the
  checkpoint's time, but the branch ran on past it, so its `.gtkw` marker is at the
  branch's own failure. The traced span is not cut at the checkpoint: a default window
  has no end, so each branch child traces to its own end.
- A fork branch killed by a signal has no failure time and an unfinished file (its trace
  was never closed, so it is often 0 bytes). Its file is listed last, labelled
  `died (killed by signal 6); file incomplete`, with no marker, and gets a `.gtkw` (and so
  a `gtkwave` command) only when it starts with a whole FST header block.
- When no model of the test can trace, the report offers no waves command, and
  `SIMJIG_WAVES_ON_FAIL` skips the replay. Trade-off: such runs lose the replay's
  determinism check under `ON_FAIL`; `SIMJIG_WAVES=1` still replays and checks.
- A passing run under `SIMJIG_WAVES=1` prints the waves block to stderr and fails the test
  if the replay did not reproduce it.
- Report: `FailureReport` gains `signals: Vec<String>` and `waves: Option<WavesReplay>`
  (`simjig::{WavesReplay, Verdict}`). Without a replay the `waves:` line is now the working
  command `SIMJIG_WAVES=1 <repro>`; 9 report snapshots changed only in that line (reviewed).
  `TestOutcome` gains `precision`; `RunEnv` gains `waves` and `trace`.

## Tests through the real pipeline (`waves_tests`, 12 tests)
- `tracing::a_traced_run_writes_an_fst_whose_header_parses` (timescale -12, vars, blocks,
  start 0, end = last timestep); `every_timestep_is_one_sample_at_its_sim_time`;
  `a_start_time_skips_the_samples_before_it`; `an_end_time_stops_sampling_after_it`;
  `a_depth_limit_records_fewer_signals`; `the_waves_of_a_failed_run_end_at_the_failure`;
  `a_model_built_without_fst_says_how_to_enable_it`; `a_fifo_stream_is_traced_too`.
- `on_fail::a_failure_with_waves_on_fail_gets_waves_from_a_replay` (counter fails at
  3000 ns: FST from 2000 ns to 3000 ns, `.gtkw` marker 3000000, report lines, `replay/`);
  `without_a_waves_variable_nothing_is_replayed`;
  `simjig_waves_window_traces_the_given_span_of_a_passing_run` (100..200 ns);
  `a_replay_that_ends_differently_is_reported_as_non_determinism` (a FakeModel DUT whose
  `limit` is the wall clock's nanosecond: fails at `limit` ns; chance of a false pass
  1e-6).
- The FST reader (`waves_tests/fst.rs`) walks the block chain to EOF, reads the header, and
  decodes every value-change block's time table (LEB128 deltas, zlib when smaller). It uses
  **`miniz_oxide` as a dev-dependency** (why not simpler: fstcpp zlib-compresses time tables
  whenever that is smaller, so exact timestamps need inflate; no zlib reader is in the tree;
  miniz_oxide is pure Rust, MIT/Zlib/Apache-2.0, plus adler2).
- `cargo-simjig::it waves_writes_the_waves_of_a_test_in_one_command` runs the real
  `cargo simjig waves on_fail::counter_counts --seed 0x2a --window 100ns..` from the
  workspace root (no `-p`, so it reuses what `nextest run --workspace` built).

## Changes outside the waves modules
- `crates/simjig/build.rs`: `counter_fst`, `fifo_fst` (shared designs with `.trace_fst(true)`)
  and a dedupe of repeated `-rpath` link args. Open in simjig-build: an FST model emits
  `-Wl,-rpath,<dir>` once for lz4 and once for zlib, usually the same directory, and ld64
  warns `duplicate -rpath` on every test binary (`linker_messages`). Users with one FST
  model see that warning; the fix belongs in `compile.rs` (dedupe the dirs).
- `model/raw.rs` (the unsafe module): `features`, `trace_open`, `trace_dump`, `trace_flush`,
  `trace_close`; the test-only fake gained a traced vtable that records calls.
- `model/meta.rs`: `LoadError::{NoTrace, TraceOpen}`. `lib.rs`: exports.
- `tests/harness_tests/runner.rs`: `..RunEnv::default()` for the two new fields.
- Root `Cargo.toml`/`Cargo.lock`: `miniz_oxide` workspace dev-dependency.

## Not verified
- Opening `waves.gtkw` in GTKWave (not installed here): the marker/zoom line follows
  GTKWave 3.3's save format from memory; vector signals are written without a `[msb:lsb]`
  suffix, which GTKWave may not match (the parent process no longer knows widths).
- Value correctness inside the FST (only timestamps, counts and header are decoded; the
  hierarchy block is LZ4 and is not read).
- Linux; a `--timing` model under tracing (the C side tests it; the Rust path is the same).
- The trace window when a test sets `SIMJIG_TRACE` and also runs several Sims in one test
  (several DUTs in one Sim get one file each, above).
- Waves for a `$fatal` cover the timesteps before the fatal only (above).
