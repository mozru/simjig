# s1c2fix: Stage 1 review cycle 2 fixes

Agent `s1c2fix`, branch `fix/s1c2fix` (from `stage/1`). Facts checked against the installed
Verilator 5.052 (pixi) unless marked 5.053-devel source.

## Probes are found through the scope table
The shim used to bind `&rootp->a__DOT__b__DOT__x`, a member that exists only when every
module is inlined into the root. Verilator 5.052 keeps a module apart (`V<prefix>_leaf.h`)
when it is large and used several times, declares a class, or carries
`/*verilator no_inline_module*/`; all three were reproduced and failed to compile.

Now `SIMJIG_PROBE(t, m, "<scope>", "<var>", "<display>", width, access)` calls
`simjig::find_probe` at create:
- `ctx.scopeFind("<model name>.<scope>")->varFind("<var>")->datap()`;
- checks: no unpacked dimension, `entBits() == width`, `vltype()` is the `VLVT_UINT8/16/32/64`
  or `VLVT_WDATA` that Verilator's storage rule gives that width;
- a failure throws, so create returns EXCEPTION with a message naming the probe.

`SignalTarget::Probe` holds `scope` (the elaborated instance path below the model) instead
of `root_member`. Facts the lowering relies on:
- scope and variable names are the plain identifiers, not C++ encodings: a scope `u__0` is
  registered as `"top.u__0"` and a variable `my__x` as `"my__x"` (`V<prefix>__Syms__Slow.cpp`);
- an interface-ported DUT's probe `iface_top.data_q` lives in scope
  `TOP.simjig_wrap_iface_top.dut` (the display path gave `TOP.iface_top`, so every probe of an
  iface DUT failed create);
- the scope prefix is the model's instance name (`opts.name`, default `TOP`); tested with
  `name=dut0`.

`lower/inlining.rs` and the reject-only `CodegenConfig::{verilator_args, control_files}` are
gone (public API change of `CodegenConfig`; there is no CHANGELOG yet). The one case a lookup
cannot serve is a probe inside a hierarchical block (`hier_block` with `--hierarchical`): that
block is a separate model and the top's scope exists but not the variable. Create fails with
a message that says so. The pure lowering cannot see the metacomment, so this is the only
place it can be reported.

Tests (`tests/codegen_it/probes.rs`, generic `probe_driver.cpp`): big module x3, class,
`no_inline_module`, `-fno-inline`, hierarchical block (expected failure), iface DUT. Each
writes the probe, evals, reads it back and reads the port it drives. All six failed before
the change (five with the C++ error or the create error quoted above, `-fno-inline` with the
old rejection).

## Coverage counters: `cov_runs`
5.052 keeps one `uint32_t __Vcoverage[N]` per module object (root, each non-inlined module
instance, each interface instance) and one in the symbol table, for counters that only static,
package and class functions increment. Each point is registered twice through
`__vlCoverInsert`: with the object-local counter and with the symbol table's counter (for the
first instance of a module) or a function-local `fake_zero_count` (later instances).

There is no C++ way to enumerate the objects, so `simjig_vl_config.h` includes
`verilated_cov.h` and redefines `VL_COVER_INSERT` to report each `countp` to the runtime whose
create is constructing the model (`simjig::note_cov_counter`). After construction create keeps
the pointers that lie inside the `V<prefix>__Syms` object (which holds the root, every module
object and its own array as members, so `fake_zero_count` drops out), sorts them by address,
deduplicates them and merges contiguous ones into runs.

ABI: offset 128 is now `const simjig_cov_run* (*cov_runs)(h, size_t* count)`, with
`simjig_cov_run { uint32_t* counters; size_t len; }` (16 bytes). The old `cov_counters` slot
had no consumer, so it was reinterpreted in place; `SIMJIG_ABI_VERSION` stays 1. Rust mirror:
`abi::CovRun`, the field table has 49 entries, `simjig_rt_abi_layout` 10 values.

How runs map to coverage points:
- runs are in symbol-table member order: the root's array, then each non-inlined module or
  interface instance in `V<prefix>__Syms` declaration order, then the symbol table's array;
  two arrays that happen to be adjacent in memory form one run;
- within an object's array, index `i` is Verilator's local bin `i` of that module (line,
  branch and user points one counter each; a `W`-bit toggle point at base `b` uses `b + 2k`
  and `b + 2k + 1`, as in `docs/notes/tracecov.md`);
- the layout is fixed for one build, not across designs or Verilator versions. The stable
  name of a point is its `coverage.dat` key; aliased signals share a counter, and points of
  several instances of one module with equal keys are summed in `coverage.dat` (so
  `tc_hier` has 9 counters in 3 runs but 3 points);
- counters outside the symbol table object (covergroup bins allocated at run time) are not
  in any run.

Tests: C++ `counters_of_every_module_object_are_runs` (`tc_hier`: 3 runs of 3, and the live
sum equals the `coverage.dat` total exactly); Rust
`coverage::cov_runs_cover_the_counters_of_an_interface_ported_dut` (`iface_top --coverage`:
at least 3 runs, a non-zero sum not above the `coverage.dat` total). The old slot returned
NULL there (checked by running the same test against `cov_counters`: `cov runs 0`).

Cost: every translation unit now includes `verilated.h` through the config header (the
model sources already did through their pch).

## One decision for timing, tracing and coverage
`DutConfig::features()` (public `Features`) decides all three once; the model build, the
JSON pass, the layout check and the shim read it. Policy, per switch:
- timing and FST tracing have builder switches, so the raw flags are refused at `validate`:
  `--timing`, `--no-timing`, `--trace`, `--no-trace`, `--trace-fst`, `--trace-vcd`,
  `--trace-saif`, `--trace-threads`, `--trace-fst-thread`. Trace tuning flags
  (`--trace-depth`, `--trace-structs`, ...) are allowed: they change nothing unless tracing is
  on;
- coverage kinds have no builder switch, so coverage flags are honoured, by Verilator's rule
  (5.053-devel `V3Options::coverage(bool)`, confirmed on 5.052 through `VM_COVERAGE`):
  `--coverage`/`--no-coverage` set all five kinds (line, toggle, expr, fsm, user), the kind
  flags one each, last word wins, coverage is on while any kind is. `--coverage-max-width 8`
  alone gives `VM_COVERAGE = 0`, and so `kCoverage = false`;
- `-G...`/`-pvalue+...` are refused (use `.param()`): they also reach the layout check, which
  fails with `Parameters from the command line were not found in the design`;
- `--threads N` is refused unless N is 1.

The compile step compares `features()` with `VM_COVERAGE` and `VM_TRACE_FST` from
`_classes.mk` and fails with a message naming the option file when they differ (a `-f` file
is the remaining way to change them). `VM_TIMING` cannot back timing up: it is 0 for a
`--timing` model without timing constructs, whose header still declares `eventsPending`.
Timing switched on inside a `-f` file is therefore not detected.

## Smaller fixes
- Non-`.vlt` `verilator_input`s are stored and passed, before the user's sources, to the JSON
  pass and the layout check as well as the model build.
- The prefix hash tags every entry with its list (`simjig-prefix-v2`): define `W=8` and param
  `W=8`, or a source and an include directory with the same path, no longer collide. Every
  model prefix changes once.
- `redirect_to_devnull` lists `/proc/self/fd` or `/dev/fd` (fallback: the old scan) and raises
  the soft `RLIMIT_NOFILE` for the call when a descriptor lies above it (`dup2` onto it fails
  otherwise). macOS caps descriptors at `kern.maxfilesperproc` (61440 here), so the test lowers
  the limit to 256 below an open descriptor 3000 instead.
- `svtypes::toolchain::{locate, judge}`: the one rule for which Verilator to run
  (`SIMJIG_VERILATOR`, `$VERILATOR_ROOT/bin/verilator`, `PATH`; a wrong explicit choice is an
  error) and whether to accept it (supported; or any release under
  `SIMJIG_UNTESTED_VERILATOR=1`, as DECISION/proposal-C describe; a non-release banner never).
  The build now honours the override (tracked variable, `cargo::warning`), and the doctor now
  searches like the build. A `VERILATOR_ROOT` without `bin/verilator` is now reported by the
  doctor's `verilator` check (fix: unset it), because the build would not look further.
- `cargo xtask ci` runs `crates/simjig-build/cpp/tests/run.sh` (about a minute).

## Not verified
- Linux: everything ran on macOS arm64 (`/proc/self/fd`, the Linux path, is untested).
- `--hierarchical` with a probe outside the hierarchical block, and probes in interface
  instances.
- A covergroup model's runs (its bins are outside the symbol table and are left out).
- Timing switched on through a `-f` file (not detected, see above).
