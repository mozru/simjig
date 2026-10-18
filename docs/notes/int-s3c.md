# int-s3c: stage/3 catching up with stage/2 and stage/1

Two merges brought the stage 2 kernel and stage 1's cycle fixes into `stage/3`, and the
s3c4 integration review then fixed the seams between them (`s3c4-fix`).

## 2257ab6: stage/2 into stage/3
Combinators (`join!`, `first!`, `with_timeout`) run each operand as a **helper task**
(cocotb 2.1's per-operand `Task`), Event and Lock waits that can complete resume after one
hop, and a Queue get or put that can complete goes ahead of woken waiters.
- Conflicts: `ARCHITECTURE.md` (the trace/waves rows and the scheduler-semantics row both
  kept); `lib.rs` `__private` (the waves re-exports kept; stage 2's `Fork`, `MaybeDone`,
  `Operand` replace the removed boxed combinator and hop).
- Conformance goldens for helpers, callback order and Event/Lock/Queue readiness came
  with it (`tests/conformance/golden/`).

## b63fdac: stage/1 into stage/3
IR schema 4 (generate blocks), the front cache (`front.rs`: IR and layout-check verdict)
and archive cache (`archive.rs`, `manifest.rs`), `VLVT_*` checks of probes, and the
`#[non_exhaustive]` growable kinds.
- Conflicts: `dut.rs` (stage 1's front end beside stage 3's DPI header check in
  `check_model`; the untested-Verilator warning moved to a helper to keep `build_in`
  short), `compile.rs` (stage 1's deps and changed-archive warnings, still filtered
  through `script::once_per_script`), `emit.rs`, `swarm.rs`, `nextest.toml`,
  `ARCHITECTURE.md`; `codegen_it` goldens and the svtypes `dpi` snapshot regenerated.
- 82b75cf then tested that a `DutConfig` shape change ignores old prefix claims.

## Seams the review found (fixed on `s3c4-fix`)
1. **Fork inside a combinator.** Helpers are not tasks (`TaskTable::unfinished` skips
   them) and `Kernel::current` is `main` while an operand runs, so `check_quiet` let
   `run_from_checkpoint` fork from inside an operand. A `join!` sibling kept running in
   each branch; a `first!` sibling that won in the child cancelled the body, and the
   child ran the rest of the test as the parent. Now `ForkError::InCombinator` when
   `main` has helpers in the table (`TaskTable::helpers_of`) or the poll's context is not
   `main`. And a guard held across the body ends the child with a failure if the fork's
   future is dropped there, so a child can never return into the harness.
2. **`dpi_in_c` and the prefix.** The prefix left `dpi_in_c` out; the claim's identity
   hashed `Debug` of the whole config, so moving an import to C failed the next build
   ("rename the DUT") until `cargo clean`. `dpi_in_c` changes only the generated Rust,
   so it is in neither now: both come from `DutConfig::model_digest`, which destructures
   every field without `..`. The claim records that digest, so a configuration that
   changes the model changes its prefix too. The cache claim moved after the front
   end's checks; the in-process name check (which still compares whole configurations,
   since the name names the generated Rust) stays first.
3. **IR schema 4 had two JSON forms** (with and without the DPI fields). Bumped to 5;
   the front-cache key already covers `SCHEMA`.
4. Prefix hashing no longer goes through `Debug` (probes and bindings field by field,
   `simjig-prefix-v3`), so `hash.rs`'s "no key hashes `Debug`" holds. `Lowered::ir_hash`
   is SHA-256 truncated, not FNV-1a; ARCHITECTURE lists build-time and run-time hashing
   as separate homes.

## Open
- `simjig` build-depends on `simjig-build` for its tests (dpi notes item 5).
- `Sim::coverage` names counters by index only; `coverage.dat` has the point names.
- `docs/notes/wiring.md` "Prefix uniqueness" still describes the `Debug` identity
  (historical notes; `pipeline::claim_prefix` is current).
- Not verified: Linux, GTKWave opening the files, `--timing` models calling DPI.
