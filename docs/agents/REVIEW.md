# Stage gate: review cycles

No stage advances without review. This is standard process.

## Flow per stage
1. The implementation agents finish on their branches. The director merges them into an
   integration branch `stage/<n>` and runs `pixi run cargo xtask ci` there.
2. **Review cycle.** At least two review agents with different personas review
   `main..stage/<n>` independently. Neither sees the other's report.
3. Each finding has a severity:
   - **Bug:** wrong behaviour, unsoundness, a constitution violation, a missing test for a
     claimed behaviour, or a broken invariant from `ARCHITECTURE.md`. *Blocks.*
   - **Risk:** likely to become a bug or a costly redesign (an API shape, a missing limit,
     a semantics gap against `DECISION.md` or the conformance goldens). *Blocks unless the
     maintainer accepts it in writing.*
   - **Nit:** style, naming, docs, small simplifications. *Does not block.*
4. Fix agents address every Bug and Risk on `stage/<n>`, and the gate runs again.
5. **Repeat** the cycle until the reviews return **only nits**. More than one cycle is
   normal. Nits are recorded in `docs/notes/stage-<n>-nits.md` for later.
6. The director fast-forwards `main` to `stage/<n>`, and only then starts the next stage.

## Reviewer personas (pick at least two per cycle; rotate them between cycles)
- **Verilator internals:** semantics checked against Verilator's source and the installed
  5.052 (eval and scheduling, public signals, storage classes, fatal paths, JSON format).
- **Rust soundness and API:** unsafe/FFI soundness, aliasing, `Send`/`Sync`, lifetimes,
  semver surface, error types, idiomatic code.
- **Maintainer:** constitution rules 1–7, simplicity, one place per concept, explanatory
  docs, debuggability, test quality (would each test fail without its fix?).
- **Adversarial user:** writes small real testbenches against the new API and reports
  what is confusing, missing or wrong.

## Reviewer output
A ranked list of findings, each with: severity, file:line, the problem, a concrete failure
scenario, and a suggested fix. End with a verdict: **pass** (nits only) or **block**.
