# simjig

Rust testbenches that drive Verilator models. The testbench owns the loop and time; the
model is a library. See `ARCHITECTURE.md` for the code map and the `agentic-rust` skill for
Rust style.

## Commands
- `cargo xtask ci`: the full gate (the same one CI runs). It must pass before you hand off.
- `cargo nextest run -p <crate>` while iterating.

## Process
Every stage passes review cycles before the next starts (`docs/agents/REVIEW.md`). Move
on only when the reviews return nits, not bugs or risks.

## Constitution
Priorities, in order: correctness > determinism/debuggability > performance > ergonomics.
Name the trade-off when two of them conflict.

1. **Prove it with a failing test.** Every fix or behaviour change adds a test that fails
   without it.
2. **Evidence, not confidence.** Report the commands you ran and their results, and say
   what you did not verify.
3. **One logical change per commit, every commit green.** Refactors go in separate commits
   from behaviour changes.
4. **Never break users silently.** Changes to the API, generated code or simulation
   semantics need a reviewed snapshot or semver diff and a CHANGELOG entry.
5. **Justify complexity in writing.** A new dependency, public abstraction, unsafe/FFI
   surface or executor-semantics change needs a "why not simpler" note.
6. **Simple shape.** Flat and deep modules; no pass-through layers. Concrete code first,
   and abstract only on the second real use. One place per concept, recorded in
   `ARCHITECTURE.md`. Functions stay at or under 70 lines.
7. **Write for a reader who wasn't there.** Each module opens with a `//!` explaining its
   concept and invariants. Comments explain domain and why, citing the rule (e.g.
   IEEE 1800 §x). Never restate the code.
