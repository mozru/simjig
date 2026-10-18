# Stage 1 nits (accepted, not blocking)

From cycle 8:
- Duplicate `-rpath` linker warnings with several FST DUTs (fixed on stage/3 by `script::once_per_script`).
- X/Z constants disappear from the generated package silently; emit a doc line from `ir::Unrepresentable`.
- "in `itop` (a `itop`)" grammar in the bad-generate-index error.
- The `<triple>/` arm of `target_dir_by_layout` is unreachable in real Cargo builds (Cargo tags `<triple>/`); caches are per triple.
- `ui::signature_errors_point_at_the_offending_tokens` and `pipeline::covergroup_models_with_mixed_coverage_fail` exceed 30 s on a loaded machine.
