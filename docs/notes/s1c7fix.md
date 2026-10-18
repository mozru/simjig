# s1c7fix: Stage 1 review cycle 7 fixes

Agent `s1c7fix`, branch `fix/s1c7fix` (from `stage/1`). macOS arm64, Verilator 5.052
(pixi), clang 21.

## `deps` listing escaping had no test (bug)
`cache::parse_deps_listing` now sits next to `deps_listing`, and
`cache::tests::deps_listings_round_trip_paths_with_escapes_and_line_breaks` round-trips
paths holding `\`, `\n` and a trailing `\r` (`str::lines` strips a `\r` before `\n`, so a
`\r` in the middle of a name would not catch a missing escape). Writing `d.display()`
instead of `escape_path(d)` fails it (5 lines for 4 paths).

## Target directory without `CACHEDIR.TAG` (risk)
Cargo writes the tag only when it creates the target directory, so `--target-dir
target/rust-analyzer` run first leaves `target/` untagged. `env::target_dir_above` now
falls back to Cargo's layout, `<target>/[<triple>/]<profile>/build/<pkg>-<hash>/out`; the
`<triple>/` level is recognised by `TARGET`'s value (Cargo adds it when `--target` is
given, even for the host triple). A build that still falls back to `OUT_DIR/simjig-cache`
prints `cargo::warning=simjig: found no Cargo target directory ... not shared`.
Tests: `env::tests::cache_dir_is_found_by_layout_in_an_untagged_target_dir` (a tagged
`target/rust-analyzer/` beside an untagged `target/debug/build/foo-0123/out`; fails with
the layout fallback disabled), `cache::an_unshared_cache_warns` (the warning reaches the
directives). Not checked against Cargo's unstable new build-dir layout.

## Nits
- (a) A good build (hit or published) of the model's verilation or model archive removes
  `OUT_DIR/simjig-unpublished/{verilated-<prefix>, archives-model_<prefix>}`. Not the
  Verilator runtime's (`libverilated-<class>`): it is shared by every DUT of a build
  script, and an earlier DUT of the same run may link it (a later DUT compiles it again,
  it is not memoised when unpublished). Those are one per runtime class and replaced in
  place by the next one kept. Same-DUT-twice in one script with an edit between the two
  builds would remove the first's model output; that run is rerun by Cargo anyway.
- (b) An archive with a source under `OUT_DIR/simjig-unpublished/` or the cache's `tmp/`
  (a model verilated while its input changed) is neither looked up nor published: it is
  kept in `OUT_DIR/simjig-unpublished/archives-model_<prefix>` without a second warning.
  Before, it was published under a key naming a one-off path, never found again.
- (b, f) `changed::an_include_edited_while_verilating_is_not_cached` asserts the obj dir
  and the model archive are under `OUT_DIR/simjig-unpublished`, `cache/archives` is empty
  after the first build and has one entry after the second, and the unpublished dir is
  empty after the second. With publishing kept, it fails at the archive check; with
  removal disabled, at the last check.
- (c) CHANGELOG: the stale "shim in `OUT_DIR/simjig/<name>/`" entry merged into the
  content-addressed shim entry.
- (d) ARCHITECTURE single home for an input edited during a run.
- (e) `CacheStatus::Miss` doc covers the unpublished case; `#[non_exhaustive]` on
  `CacheStatus` and `SeedSource` with `E0004` compile-fail doctests (no new variant).
- (g) C++ `a_real_is_refused_by_its_type` also binds `rv` as 64 bits
  (`Vtc_plain_real64`). Checking the width before the type fails it ("MUTANT width first"
  on `tc.u.rv`).
- (h) nextest: the three named tests and `fixture_crate::*` (45-54 s in the gate) get
  `slow-timeout = 60s x 6`. Only `rebuild::a_compiler_changed_in_place_reruns_the_build_script`
  passed 60 s (63 s), already under the `rebuild::` override.
- `archive.rs`: publishing a compiled archive moved to `fn publish` (refactor commit) to
  keep `cached_archive_keyed` under 70 lines.

## Not verified
Linux; a real `--target-dir target/rust-analyzer` session (simulated in the unit test);
`OUT_DIR` on another filesystem than the cache.
