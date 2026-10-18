# s3c10-fix notes

## Option files are bytes
`optfile` reads `std::fs::read`, splits lines at `\n` only and uses C's ASCII `isspace`
(space, `\t`, `\n`, `\v`, `\f`, `\r`), as `V3Options::parseOptsFile` does with
`std::getline` and `std::isspace`. Checked against the installed 5.052 by reading words
back from `Cannot find file containing module`: `r2\` + CRLF is `r2\r`; `"q` CRLF `x"`
is `q\r x`; `\v` and `\f` split; `\xa0`, `\x85`, `\x1c` do not. The reviewer's `tok/`
cases c1-c8 give the same words as 5.052 (5.052 prints `a5//x` as `a5/x`: its path
cleanup, not its tokenizer).

Words stay bytes; checks see them as lossy UTF-8 (option names are ASCII). Paths are
the exact bytes (`OsStr::from_bytes`, unix). A non-UTF-8 *path* is refused
(`optfile::untracked`): Cargo directives are UTF-8 lines, so `rerun-if-changed` cannot
name it, and an edit would never rebuild. Trade-off: correctness over ergonomics; a
Latin-1 comment or `+define+` value is fine. Not covered: a non-UTF-8 path given to
`.sources()` directly, or one in Verilator's `.d` file (the built path's reruns use
`Path::display`, unchanged).

## `refused_arg`
5.052 has no `=` forms (`--prefix=Vx` is its own `Invalid option`), so they are no
longer refused. `--threads` is refused only when C's `atoi` of the value is above 1
(checked: 5.052 accepts `01`, `+1`, `1x`, ` 1`, and `0` with a DEPRECATED warning).

## Interface parameters follow the module's
Verified first: `.param("W", 12)` with an 8-bit `acc_if` and `-Wno-fatal` built, the
top's `up_d` 8 bits and `q` 12 bits (WIDTHEXPAND only as a warning); without
`-Wno-fatal` it fails on lint. Chosen: (b) plus (a). `pipeline::elaborate` adds each
`.param` that a bound interface declares and its binding leaves unset to the binding,
rerunning the elaboration pass only then (why not simpler: (a) alone forces every user
to repeat the width, which is how the mismatch happens); then a binding value that
elaborates differently from the DUT's same-named parameter is refused. The rule is by
name: an interface parameter that only shares a name with a module parameter cannot be
set apart from it. Changes the generated wrapper for DUTs that relied on the interface
default (CHANGELOG).

## Override errors
`unknown_params` now also runs on `Instance attempts to override` (a localparam, a
variable) and checks binding params against the bound interface in the override-free
pass. Localparams come from `Module::variables` (`VariableKind::Localparam`).

## Skipped
The optional `+define+` hint for a failing line from a file-list define: not cheap
(needs mapping a Verilator location back to a define's origin).
