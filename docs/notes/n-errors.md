# n-errors: the interface-default hint; reruns before a build error

## Done
- A width diagnostic (`%Warning-WIDTH*` under the default fatal lint, or `%Error-WIDTH*`
  from `-Werror-`) on a DUT whose module and a bound interface default a shared parameter
  differently now leads the error with a `hint:` naming both defaults
  (`pipeline::IfaceParamMismatch`, `pipeline::hint_width_error`). A failed JSON pass wrote
  no IR, so its mismatches come from rerunning the elaboration top with `-Wwarn-WIDTH*`
  appended (a later `-Wwarn-` beats an earlier `-Werror-`, checked against 5.052).
- `Error::Command::hints`, boxed to keep `Error` under clippy's `result_large_err`.
- `Dut::run` split (`Dut::compile`) to get under 70 lines.

## Not done: dropping the `rerun-if-changed` wall before a build error
Checked with a scratch crate on cargo 1.99.1:
- A build script that exits non-zero has its **whole** stdout dumped under `--- stdout`,
  `cargo::` lines included, before `--- stderr` (where the error is).
- Cargo reruns a failed build script on every build whatever it printed: two failing
  `cargo build`s ran the script twice with no change.

So a failed DUT's reruns are needed only when the script handles the error and exits 0.
They are useless when it propagates the error. But `Dut::build` returns before the
script decides, and nothing in the process learns the exit status later: Rust runs no
static destructors, and `atexit` gets no status. Each way to hold the lines back trades
correctness (a handled failure that never reruns when fixed) for ergonomics:
- **Flush on the next `Dut::build`.** Correct for multi-DUT scripts, but it loses the
  reruns when the last DUT fails and is handled, which is the case
  `rebuild::s3c6r_*` and `optional.rs` test.
- **Flush on `Drop` of the `Error`, unless it was `Debug`-formatted** (which `main`'s
  `Termination`, `unwrap` and `Box<dyn Error>` do) or the thread is panicking. A `Drop`
  impl on the public enum breaks by-value field moves out of it (E0509). A handled
  error logged with `{e:?}` would silently lose its reruns. `anyhow` formats with
  `Display`, so its users would keep the wall.
- **An explicit "handled" call** (for example `e.handled()`, or `Dut::build_optional`).
  Existing handled scripts would silently stop rerunning when fixed (rule 4).

Earlier DUTs' success directives (link arguments, and a `rerun-if-changed` per file
Verilator read) are part of the wall too, and no failure-path change removes them.

A zero-risk partial option: print the failing DUT's headline as a plain stdout line
before its reruns. Cargo ignores it on exit 0, and it is the first line of the failing
DUT's part of `--- stdout`.
