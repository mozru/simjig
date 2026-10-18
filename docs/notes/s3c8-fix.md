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
