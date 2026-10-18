# s3c9-fix notes

## Reruns: one rule
`dut::reruns` over `dut::tracked_files` now serves the built and the failed path. The
success path had emitted `CppRecipe::include_dirs` raw, so `"."` made Cargo scan the
package and its `target/` on every build. Chosen over "track only the `-MD` headers"
because the C++ compile's dependencies are `cc`'s, not Verilator's `-MD`, and the
failure path has neither; one rule for both keeps one home.

## Option files as 5.052 reads them
Checked by running the installed 5.052 on option files and reading the words back
from `Invalid option: ..` (the 5.053-devel source agrees): `\` escapes, `"..."` with
escapes, `'"..."'` keeping the double quotes, a lone `'` keeps the next character
(even a line's closing space: `'--bog us'` is `'--bog` and `us' `), and
`// verilator fargs` / `/* verilator fargs */` replace the file's words. The old
module docs claimed quotes and escapes were not interpreted; they were wrong for 5.052.

## Refused options in option files
`validate` runs `refused_arg` over every option-file word (`optfile::find` now takes a
closure that also gets the next word, for `--threads 1`). A list file is called a
"file list" in the message, an `-f` among `verilator_args` a "Verilator option file".

## Misspelled `.param`
Why a second JSON pass, not a check before verilating: the IR comes from the JSON pass,
which is the run that fails on the bad override (`-G`: "Parameters from the command line
were not found"; interface ports: `PINNOTFOUND` in the generated elaboration top). On
that failure only, `pipeline::unknown_params` reruns the pass without the overrides
(through the elaboration top for interface ports) and compares names. When that pass
fails, or every `.param` exists, Verilator's error stands. Message:
``invalid DUT `rl_small` configuration: module `rate_limiter` has no parameter `PERIDO`;
did you mean `PERIOD`? (it has BURST, PERIOD)``: the review's wording, with the
`Error::Config` prefix every configuration error has.

## Smaller items
- Command failures: the headline is ``<what> failed (<status>) at <file:line:col>:
  [CODE: ]<message>``; the excerpt follows without that line. In the long form (over 40
  lines) the line can still show again in the "first 40 lines" context.
- The JSON pass reads as ``elaborating DUT `x` ``.
- Package hint on `Can't find typedef`, `Package/class for .. not found` and
  `.. before declaration (IEEE 1800-2023 6.18)` (5.052's messages for each case;
  `import p::*` of a package read later or not at all gives the last one).
- `Dut::bind_iface_with_params(port, "iface.modport", [(name, value)])`, values `Display`.
- The DUT name is claimed before `validate`, so a validate failure still owns its stub.
  A second same-name DUT with another configuration after a failed one is now refused,
  as it already was after any later failure.
- `simjig::log!` is now `simjig::tb_log!` (the `log` crate exports `log!`).
  `docs/architecture/DECISION.md` and the proposals still say `simjig::log!`; they are
  the decision record and were left as written.
