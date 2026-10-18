# s3c7-fix notes

## A failed DUT: stub plus `cfg`
On `Err`, `Dut::build_in` (and `Dut::build` when `BuildEnv::from_cargo` fails but
`OUT_DIR` is set) rewrites `OUT_DIR/simjig/<name>.rs` as one
`::core::compile_error!("DUT `<name>` failed to build ...\n<error>\n...")` (`emit::stub_failed`).
A built DUT adds `cargo::rustc-check-cfg=cfg(simjig_dut_<name>)` and
`cargo::rustc-cfg=simjig_dut_<name>` to its directives; a failed `build()` prints the
`check-cfg` only, so `#[cfg(simjig_dut_<name>)]` raises no `unexpected_cfgs` either way.
Cargo 1.99 (pinned) supports both directives; `rustc-cfg` reaches every target of the
package, integration tests included.

Why both, and why not simpler:
- Deleting the stale file gives "couldn't read .../x.rs", which does not say the DUT
  failed. The stub costs the same and names the DUT and its error.
- Detecting the stub in `include_dut!` would need a marker the macro can test at expansion
  time; the stub's own `compile_error!` already is that.
- The stub alone gives no way to build *without* the DUT. A `cfg` gates every item that
  names the model (the include, tests, helpers); letting each user print their own cfg
  from the `Result` works too but every script would repeat it, and `check-cfg` on the
  failure path is easy to forget.
- No stub when an earlier DUT of the same build script claimed the name
  (`script::claimed`): that DUT owns the file (a second, conflicting `Dut::new("x")`
  would otherwise clobber a good module). No stub for a non-identifier name.
- Writing the stub is best effort: the build already failed with the real error.

Verified that without the stub, the fixture (`tests/it/optional.rs`) fails with
`Undefined symbols for architecture arm64`, and the three `pipeline::*stub*` tests fail.

## Failed-build reruns
`optfile::inputs` replaces `optfile::files`: the option files plus the paths in them and
in `verilator_args` (positional words with a source suffix, `-v`, `-y`, `-I<dir>`,
`+incdir+`), resolved like `V3Options::parseFileArg` (`-F`: relative to the file's
directory). Positional words are recognised by suffix because only Verilator's option
table knows which options take a value (`--timescale 1ns/1ps`). Not tracked: a source
without a known suffix and `$VAR` in paths (README).

## TIMESCALEMOD (Verilator 5.052, checked by hand)
An untimed package is flagged ("Timescale missing on this module"); an untimed interface
used by a timed module is not. README corrected. The hint names each flagged unit as
`file:line` and the `--timescale` default.

## Debug of times
`Elapsed` carries its precision, so its `Debug` prints `Elapsed { after: 3 ns, at: 5 ns,
precision: 1ns }`. `SimTime`/`SimDuration` carry none; reading the running simulation's
precision inside `Debug` would make the output depend on where it is printed, so they
keep their tick `Debug` (documented). No golden or snapshot contained `Elapsed`'s Debug.

## The panic printed twice
Documented, not suppressed (`kernel::panics`, quickstart README): silencing the chained
hook inside a run would drop `RUST_BACKTRACE` output, a user's own hook and the only
trace of a panic that escapes `Sim::run` (foreign wakers, `kernel::waits`).

## Fork test under `cargo test`
`random_continuations_from_full` returns after printing the `ForkError::Threads` message
when the process has more than two threads, rather than detecting nextest from `NEXTEST`:
the thread count is what the fork actually requires.
