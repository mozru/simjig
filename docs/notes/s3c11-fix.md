# s3c11-fix notes

## Interface parameters: a binding value wins, a `.param` is a default, a mismatch warns
The director's decision on the two reviews of cycle 10. Supersedes the refusal in
`s3c10-fix.md`.

The rule, for a parameter `P` that the DUT module and a bound interface both declare:
1. a value the binding sets (`bind_iface_with_params(.., [("P", v)])`) is used as is;
2. otherwise a `.param("P", v)` is passed to the binding too (`pipeline::elaborate`,
   unchanged from cycle 10);
3. otherwise both keep their own defaults, and when the elaborated values differ the
   build emits a `cargo::warning` (`pipeline::iface_param_warnings`) naming both values
   and the `.bind_iface_with_params(..)` that settles it.

Why only a warning: IEEE 1800-2017 §25.5 interface port headers carry no parameter
assignments, so the name match is a heuristic. The soundness reviewer's
`widen` (an `out` interface twice as wide as `W`, read through the port) is correct
SystemVerilog that the cycle-10 refusal broke; it is now a regression test
(`errors::an_explicit_interface_param_is_not_refused_by_name`). Trade-off:
correctness (no false refusal) over enforcing the common case; the warning covers
the usability reviewer's silent `-Wno-fatal` build.

The warning is computed from the IR the emitter sees (the wrapper's, which holds the
DUT and each bound interface specialisation), not from the elaboration pass, so a
front-end cache hit warns too (tested by building twice). Only representable
parameters are compared, by their displayed value.

## The lint hint
Leads with `-Wno-<CODE>` and `lint_off`; says `-Wno-fatal` also hides interface/module
width mismatches. The quickstart `build.rs` comment follows.

## Option parsing against 5.052
- `-no<opt>` without the dash: `V3OptionParser::find` accepts it for every on/off
  option after an exact match fails (`act.first == nop || act.first == "-" + nop`).
  `on_off` strips `no-` then `no`; callers only match on/off base names, so exact
  options such as `-no-trace-top` stay themselves. Checked: `--notiming`,
  `-notiming` (STMTDLY "due to --no-timing"), `--nocoverage-line`, `--notrace`.
- A NUL ends its option file line: `parseOptsFile` walks `line.c_str()`. Checked:
  `aa\0bb cc` / `dd /* x\0 */ ee` / `ff */ gg` reads `aa`, `dd`, `gg`.
- `--threads` out of `int` range: macOS libc's `atoi` is `(int)strtol`. Checked:
  `4294967297` gives `threads() { return 1; }`, `4294967298` gives 2, `4294967296` and
  `-99999999999999999999` the deprecated 0, `2147483648` "must be >= 0". glibc does the
  same; not run on Linux here.

## Other nits
- `cargo simjig <cmd> --help`, `-h` and `help <cmd>` print that command's help to stdout
  and exit 0 (help had gone to stderr; `doctor --help` was exit 2).
- `Starved` help names `Timer::new(ns(1)).await` for combinational designs; the README
  says a combinational design advances with a `Timer` per vector.
- The README's Rng/Queue lines are type-checked verbatim in
  `crates/simjig/tests/harness_tests/readme.rs` (compiled, never run: they are a sketch).
- A library target is fine (since `compile.rs` emits `rustc-link-arg`); the quickstart
  README and `glue.md` item 1 say so.
