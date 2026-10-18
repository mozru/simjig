# Stage 3: accepted nits

Stage 3 passed review at cycle 12. Both reviewers (maintainer / integration
soundness, and RTL + DV usability) reported nits only. Those nits, and the earlier ones
left open, are recorded here for later work.

## Cycle 12
- `pipeline.rs` interface-parameter warning: name the change-free way to silence it,
  binding the interface's own value (`[("WIDTH", 8)]`), next to the fixes that change
  a width.
- The warning is computed from the IR, so under Verilator's default fatal lint a
  default-width mismatch stops at `WIDTHTRUNC` and the friendlier warning never shows.
  Print it on the failure path too.
- `differing_interface_parameter_defaults_warn` does not assert that its second build
  was a front-end cache hit.
- `cargo simjig help help` and `help a b` report "no command `help`".
- The `.gtkw` written on failure has no signals pre-added.
- A generic `stream_if #(W)` used for two differently sized ports shares no name with
  `W_IN`/`W_OUT`, so the name rule neither ties nor warns. This is documented as a
  heuristic.

## Earlier cycles, still open
- A `scope` task's early exit with an unrun handle fails as `Setup`. Only `main`'s
  returning poll is exempt; this is documented.
- Build errors are still preceded by Cargo's `rerun-if-changed` lines.
- There is no `From<u8..u64>` for `Bits` (it would break literal inference); use
  `u128::from(byte)`.
- Packed structs have no `to_bits()` or `LowerHex`. There is no exact narrowing getter.
- A failing equality between `Bits` and `u128` prints the two sides in different formats;
  the README recommends `Bits::new(want)`.
- The panic message is printed twice: once by std, once in the report.
- No hint ties a bad `+define+` in a file list to the line that fails.
- Rerun tracking does not track sources without a known suffix, `$VAR` paths, files
  added later to a tracked directory, or subdirectory includes.
- A parameter sweep needs the README trait pattern. Generated types are not shared
  between configurations.
- Randomized initial register state needs a `ModelOptions` change
  (`+verilator+rand+reset+2`).
- Not verified on Linux. CI covers macOS only.
