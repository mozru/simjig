# s1c6fix: Stage 1 review cycle 6 fixes

Agent `s1c6fix`, branch `fix/s1c6fix` (from `stage/1`). Checked against Verilator 5.052
(pixi, conda-forge) and clang 21 on macOS arm64 (APFS, nanosecond timestamps).

## Inputs edited during a run poisoned the cache (bug)
All three two-step caches (verilation `cache.rs`, IR `front.rs`, archives `archive.rs`)
hashed the files a run read after the tool exited, so an edit during the run cached the
old output under the new contents' key, a hit forever. Fix, the ccache way, in
`manifest.rs`: a run takes a `Snapshot` before it starts (its start time and the current
digests of the previous manifest's files); `Manifest::record` returns
`Recorded::Changed(path)` when a dependency's mtime or ctime is not before the start, its
digest differs from the snapshot's, or it vanished. The caller then uses the output for
this build only: the work dir moves to `OUT_DIR/simjig-unpublished/<label>` (it must
outlive the build script, the link reads it; if the rename fails across filesystems it
stays in the cache's `tmp/`), the build prints `cargo::warning=simjig: DUT `x`: `path`
changed while <verilating | the front end ran | compiling `lib`>...`, and the next build
runs again (Cargo reruns the script: the edited file is newer than its last start).
- The start is the earlier of the system clock and the mtime of the work directory just
  created: Linux stamps files with a coarse clock that can lag `SystemTime::now()` by a
  tick, which would let an edit right after the start look older.
- Contents are hashed before the times are read, so an edit in between is caught.
- The front window is from its lookup to the end of the verilation; a front hit only
  checks when it writes a new layout-check marker.
- A libverilated that was not cached is not memoised either (`RUNTIMES`).
- Verilation manifests now use `Manifest` (digest + trusted status) instead of a plain path
  list; the entry's own `deps` file keeps the full list with `verilator_bin`, which the key
  leaves out. Domain `simjig-verilate-deps-v2`: every model verilates once more.
- Known false positives: a file whose time is in the future (clock skew) is never cached
  (the warning says so); on a coarse-clock filesystem a file written in the same tick as
  the start counts as changed (one extra rebuild).
Tests (each failed with the check disabled): `changed::a_header_edited_while_compiling_is_not_cached`
(a ccache stand-in rewrites `k.h` after compiling the source that includes it; the second
build's archive has the new symbol), `changed::an_include_edited_while_verilating_is_not_cached`
(a `SIMJIG_VERILATOR` wrapper rewrites `defs.svh` after `--cc`; the second build verilates
again, then hits), `changed::an_include_edited_during_the_json_pass_is_not_cached` (the
wrapper rewrites a `string` parameter's include after `--json-only`; the second build's IR
has the new value), plus `manifest` unit tests for the time and digest rules.

## IR recorded before the model check (bug, second reviewer)
`front.record` ran before `check_model`, so a build that failed the header, probe or DPI
check left its IR cached. It now runs after. `probes::a_failed_model_check_records_no_ir`.

## Model archives shared across target dirs (nit, done the preferred way)
The shim is stored content-addressed (`inputs/<hash>/<prefix>_shim.cpp`) and compiled
from there, so its path in the archive's key is the same in every target dir:
`cache::second_build_hits...` now checks another `OUT_DIR` links the same model archive
(it compiled a second one before). With no generated source left in `OUT_DIR`,
`Toolchain::is_generated` was only skipping listings that never change (a cache dir holds
one immutable entry), so it is gone. `OUT_DIR/simjig/<name>/` is no longer written.

## Other items
- `-G`/`-pvalue+` inside option files is refused with the `.param()` hint (`optfile.rs`:
  Verilator's comment rules, `-F` relative to its file, nested files, a depth limit; quotes
  are not interpreted, `$VAR` not substituted).
- Comment-only `.svh` edits recompile dependent archives: the model sources sit in the
  verilation's entry, whose path is in the archive's pre key. Accepted and documented
  (`archive.rs`); ccache absorbs the compile when present.
- `find_probe` checks the storage type before the width, with `syms.rs`'s words; C++ test
  `a_real_is_refused_by_its_type` (`tc_sub.rv`, a `real`, bound as 1 bit). It failed with
  the type check disabled ("1 bits wide, but Verilator stores it as VLVT_REAL").
- nextest: the `rebuild::` tests share a test group (`max-threads = 1`, so neither waits on
  the other's lock while its clock runs) and may take 6 minutes. Cold run (fixture target
  removed): 37 s and 8 s inside the full gate.
- Tests for the remaining key parts: a new file in an include dir reruns the JSON pass, a
  different layout-check text reruns the lint (both failed with that key part removed), the
  trust margin is strict, a file a second old is not trusted.
- Manifest and `deps` listings escape `\`, `\n` and `\r` in paths (`simjig-manifest-v2`).
- `#[non_exhaustive]` on `simjig::{FailureKind, TraceKind, ClockError, TimeError}`, with
  `E0004` compile-fail doctests; the swarm test's match got a wildcard arm.
- ARCHITECTURE.md single homes: the `Packed::Raw` storage copy and the C ABI mirror.

## Not verified
Linux and coarse-timestamp filesystems (the clock-lag reasoning is from the kernel's
behaviour, not a run); `OUT_DIR` on another filesystem than the cache (the `tmp/`
fallback); non-UTF-8 paths in listings (written lossily: a miss, not a wrong hit).
