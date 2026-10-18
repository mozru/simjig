# s3c8-fix notes

## `Dut::file_list`: why not simpler
- *Count an `-f` among `verilator_args` as sources.* It removes the "no sources" error but
  not the order bug: extra arguments go after the sources, so a package in the list is
  read after a source importing it. Moving every `-f` in `verilator_args` before the
  sources would change what existing option files (lint flags placed last on purpose)
  mean.
- *Expand the list ourselves into `.sources`.* It would drop the options a list carries
  (`+incdir+`, `-D`, `-y`) or need a second copy of Verilator's option parser, and the
  cache would hash our expansion instead of what Verilator read.
- So the list is passed to Verilator as `-f`/`-F` in one place, `JsonPassOpts::design_args`
  (after `-G`, before the sources), which every run already uses. The cache keys see it
  for free: the pre keys hash every absolute file among the arguments (the list itself),
  and Verilator's `-MD` dependencies list the list and the files in it
  (`V3File::new_ifstream` calls `addSrcDepend`). `optfile` sees the lists through
  `DutConfig::option_args` (`-f`/`-F` first, then `verilator_args`), the order Verilator
  reads them.
- `-F` is `file_list_relative`, not a flag argument: two words read better in a builder
  than `file_list(path, true)`.

## One option-file traversal
`optfile::find` and `optfile::inputs` walked option files separately, which is why the
nested-`-f`-in-`-F` resolution bug (reviewer bug 1) existed twice. Both are now visitors
of `optfile::traverse` (refactor commit first, then the fix with its test).

## Failed-build directory tracking
A directory is tracked by its direct files (`dut::tracked_files`), never an ancestor of
`OUT_DIR`. Lost: a file added later to an `-y`/`+incdir+` directory, and files in its
subdirectories that an `` `include "sub/x.svh" `` reaches. The success path is unchanged:
it tracks what Verilator's `-MD` says it read, plus the recipe's include directories
(still tracked whole; a C++ include directory containing `target/` would loop the same
way, but none of the reviewers' shapes has one).

## The buried `%Error`
The brief asked to wrap the generated tops in `lint_off` for the codes they trigger. The
elaboration top triggers `PINMISSING` (one per open data port) and, in the user's
interface file, `UNDRIVEN` (its signals are undriven there). Waiving `PINMISSING` in the
file broke `iface_diag`, which reads an unbound interface port's declaration from that
very warning (two `ifaces::` tests failed), and a `lint_off` cannot reach the `UNDRIVEN`,
located in the user's file. So the JSON pass's error drops the warnings located in the
generated tops it read (`error::without_warnings_in`) instead: correctness of the
binding diagnostics over the simpler waiver. The wrapper (the model's top) triggered no
warning under `-Wall` in the shapes tried, and keeps only its `TIMESCALEMOD` waiver: the
model build must still report a wrapper warning, which would be simjig's bug.

The excerpt leads with every `%Error` line (at most 40) when the stderr is longer than
40 lines, then the first 40 lines. The first line of the message carries the first
error, for scripts that print only `e.to_string().lines().next()`. The lint hint keyed
off "no `%Error` except `Exiting due to .. warning(s)`", and `.. 1 error(s), N
warning(s)` also ends in `warning(s)`: fixed (`error::is_error_line`).

## Parameter sweeps: why documentation (c), not shared types (a) or a trait (b)
- (b) A generated per-top trait (`PktParserPorts`) cannot have one signature per port:
  a struct or enum port's type is generated per DUT (`pkt_small::Hdr`, `pkt_big::Hdr`),
  and a parameter-sized port is `Bits<4>` in one and `Bits<64>` in another, so most
  methods need associated types (`type Hdr: Packed`), and the interface bundles too.
  The trait's shape would change with every parameter that reaches a width, which is
  a new public abstraction in the generated code whose stability we cannot promise, plus
  a naming rule for the trait and a home for it (each DUT's module is included
  separately, so neither owns it).
- (a) Shared package types need the generated modules of one build script to agree
  across DUTs: compare the type IR of every DUT of a top, emit a third module both
  include, change `include_dut!` (or add a second macro) so the shared module is
  included once per crate, and keep `cargo simjig show`, the IR hash and the layout
  check per DUT. That is a change to the generated code's layout and to the
  include contract, larger than the reviewer's problem warrants, and it helps only when
  the types are identical (the brief's own condition), not a width that follows a
  parameter.
- (c) The README's "A parameter sweep" shows a user trait implemented per `Dut` by a
  macro that holds only the impl, generic test bodies, and one `#[simjig::test]` per
  configuration: failure locations point at the generic body. Checked by compiling and
  running it against the reviewer's two-config parser (both tests pass; a deliberately
  wrong stimulus failed at `sweep.rs:66`, the generic body). Revisit (a) if sweeps
  with many shared types become common.
