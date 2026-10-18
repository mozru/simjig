# Stage 2 nits (accepted, not blocking)

From review cycle 3 (Rust soundness + first-time user):
- `DeltaLimit` doesn't name the writing task.
- A by-value `TestCtx` parameter gets a misleading macro error; say "take `&simjig::TestCtx`".
- `SimTime` has no `Display` (no precision in the test body).
- The `let _ = spawn` warning is invisible under nextest capture; consider surfacing it in summary.json.
- `Bit` lacks `to_u128`.
- The repro `--manifest-path` is an absolute path (machine-specific by design).

From review cycle 5:
- `cycles` has no `Out` (design-driven clock) form; use a loop of `rising()` or add `Out::cycles`.
- `Queue::new` needs a capacity, while cocotb's defaults to unbounded.
- The swarm seed count is hard-coded (400); add `SWARM_SEEDS`.
- The combinator docs overstate when a fork starts (`Fork::poll_in` starts helpers on first poll).
- In a hand-written join, a ready `ev.wait()` next to a combinator may progress in a helper run ("may progress", not "tolerated").
- Cancelling deep nests costs polls quadratic in depth (5,456 polls for 100 levels).
- `poll_task` is at 69 lines.
- Pin the `Task`-by-value known difference with a test (`r5_task_operand`).
