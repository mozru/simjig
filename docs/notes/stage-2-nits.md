# Stage 2 nits (accepted, not blocking)

From review cycle 3 (Rust soundness + first-time user):
- `DeltaLimit` doesn't name the writing task.
- A by-value `TestCtx` parameter gets a misleading macro error; say "take `&simjig::TestCtx`".
- `SimTime` has no `Display` (no precision in the test body).
- The `let _ = spawn` warning is invisible under nextest capture; consider surfacing it in summary.json.
- `Bit` lacks `to_u128`.
- The repro `--manifest-path` is an absolute path (machine-specific by design).
