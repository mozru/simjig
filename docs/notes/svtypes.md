# svtypes notes

Design notes, measurements and surprises from building the `svtypes` crate against
Verilator 5.052 (conda-forge, pinned in `pixi.toml`).

## Public API
- `svtypes::ir`: `Design`, `Producer`, `TimeExp`, `TypeId`, `Type`, `TypeKind`
  (`Bits`, `Struct`, `Union`, `Enum`, `PackedArray`, `UnpackedArray`), `Range`, `Field`,
  `Variant`, `ModuleId`, `Module`, `ModuleKind`, `Package`, `Param`, `ParamValue`, `Port`,
  `Direction`, `Storage`, `IfacePort`, `Modport`, `ModportMember`, and `SCHEMA` (now 1).
- `svtypes::literal::{Literal, LiteralError}`: canonical two-state literals.
- `svtypes::width`: range parsing, packed widths, struct offsets, `packed_storage`.
- `svtypes::frontend::parse(tree_json, meta_json, version) -> Result<Design>`.
- `svtypes::verilator::{json_pass, write_json_files, JsonPassOpts, JsonFiles}`.
- `svtypes::error::{Error, Result}`.
- The `svtypes dump <sv files> --top X [-G N=V] [-D N[=V]] [-I DIR] [-- ARGS]` binary.

## IR decisions
- **Typedef chains.** A typedef that renames another type is its own `Type` with a
  `name` and `alias_of` (one step of the chain). Its `kind` is a copy of the target's, so
  consumers never chase aliases unless they want the chain. A typedef of an anonymous
  struct or enum does not add an alias: Verilator already names that struct or enum
  after the typedef (`pkg::inner_t`).
- **Type names** are qualified by scope: `pkg::t`, `$unit::t`, and `mod::t` for
  module-local typedefs. The scope comes from matching the `REFDTYPE` against the
  `TYPEDEF` nodes of every module and package (same name, same target `addr`), falling
  back to `classOrPackagep`.
- **Type parameters** (`parameter type T`) are transparent: a port of type `T` gets the
  bound type. They are not listed in `Module::params`.
- **Synthetic typedefs.** Verilator invents `__typeimpmod<N>`, `__typeimpsu<N>` and
  `__typeimpenum<N>` typedefs for anonymous struct and enum types (`V3LinkParse.cpp`,
  `verilog.y`). They are dropped, so an anonymous enum port has an unnamed `Enum` type.
- **Specialisations.** Each clone is one `Module` with `name` (Verilator's mangled
  `sub__N3`), `orig_name` and its `params`. Modules are sorted by `(orig_name, name)`.
  Even non-parameterised modules get cloned when they have interface ports
  (`producer__Bz1`).
- **Module params** list only `parameter`s (GPARAM). Module localparams are internal and
  skipped. Package params list both kinds.
- **Param values**: `Bits(Literal)` retyped to the parameter type's width and signedness
  (a `-GW=16` override arrives as unsigned `32'h10` although `W` is `int`), `String` and
  `Real` (decimal text). Array-valued parameters (`INITARRAY`) are an error for now.
- **`cpp_member`** is `Some` only on the top module. It is the encoded `origName`
  (`\a$b` becomes `a__024b`), prefixed with `__SYM__` when it is one of the words
  `V3LanguageWords` reserves (`char` becomes `__SYM__char`). Verilator's `V3Name` applies
  that rename after the JSON dump, so the JSON shows the bare name. The word list is
  copied from the 5.053-devel source; a rename list change in a later Verilator would show
  up in the header cross-check test.
- **`Storage`**, checked against generated `V*.h`: `CData` ≤ 8, `SData` ≤ 16, `IData` ≤ 32,
  `QData` ≤ 64, `VlWide<ceil(w/32)>` above. Packed structs, unions and packed arrays are
  stored by total width (`VL_IN(&o,18,0)` for a 19-bit struct). Unpacked arrays are
  `VlUnpacked<elem, len>`, nested outermost first. The model's port members are
  references (`VlUnpacked<CData, 4> &ua;`, and the `VL_IN*` macros expand to references
  too). Unpacked struct ports have no supported storage and are an error.
- **Time precision** is a power-of-ten exponent (`TimeExp(-14)` = `10fs`), taken from the
  netlist's global `timeprecision`.

## Facts about the 5.052 JSON (beyond the brief)
- Ports: any `VAR` with a `direction` other than `NONE`. `inout wire` ports have varType
  `WIRE`, not `PORT` (the header cross-check caught this).
- Interface ports: `VAR` with varType `IFACEREF` whose `IFACEREFDTYPE.cellName` is empty.
  The wrapper's interface instance also gets an `IFACEREF` `VAR` (`bus__Viftop`), with
  `cellName` set; it is not a port.
- Modport members are `MODPORTVARREF {name, direction, varp}`. Other modport items
  (tasks) are rejected.
- The top module has `level: 1`. There is no `resolvedTopModuleName` in 5.052 (the
  5.053-devel source has it).
- Signed `-G` and default values differ: `parameter int P = 3` prints `32'sh3`, `-GP=5`
  prints `32'h5`.
- String constants print as `\"text\"` (backslash, quote).
- `integer` and `time` are 4-state; `reg` and `wire` print as `logic`.
- Struct members carry `refDTypep` (the typedef chain) and `dtypep` (resolved). We use
  `refDTypep` to keep names.
- `MEMBERDTYPE.lsb` is `"-1"` in unpacked structs.
- `.tree.meta.json` contains heap addresses (`pointers`), absolute paths and a
  `ptrFieldNames` list printed from a hash set. The regen test drops `pointers`, reduces
  paths to file names and sorts `ptrFieldNames`. `.tree.json` itself was byte-identical
  across two regenerations.
- A `// Verilator ...` line comment is parsed as a pragma (`BADVLTPRAGMA`). Fixtures avoid
  starting comments with that word.
- `--json-only` with `--top-module` naming a module with interface ports fails with
  `Unsupported: Interfaced port on top level module`.

## Strictness
Every dtype node reached from a port, parameter, typedef or modport is checked field by
field against a per-node list (`check_fields`). Unknown fields are errors, except empty
child lists, which carry nothing. Unknown dtype node types are errors. Errors name the
JSON pointer, the source location (from `.tree.meta.json`) and the Verilator version, e.g.
`Verilator 5.052 JSON at /miscsp/0/typesp/6 (design.sv:5:11): unknown field `isTagged` on
STRUCTDTYPE`. Nodes that no port or type reaches (statements, classes, the `std`
package) are ignored.

Widths are cross-checked where Verilator prints something: member `lsb` and packed
`isFourstate` must match the computed values.

## Runner
`json_pass` runs `verilator --version`, then `verilator --json-only --no-timing -Wno-fatal
--top-module X -Mdir <tmp> --json-only-output ... --json-only-meta-output ...`. `-Wno-fatal`
is deliberate: lint policy belongs to the model build, which sees the same warnings.
The temp dir is created with `std::fs::create_dir` (atomic) instead of the `tempfile`
crate. Errors carry the command line and the last 40 lines of stderr.

## Dependencies (why not simpler)
- `serde` + `serde_json`: the IR's serialized form and reading Verilator's JSON. Expected
  by the brief. `serde_derive` pulls `unicode-ident`, licensed
  `(MIT OR Apache-2.0) AND Unicode-3.0`, so `deny.toml` now allows `Unicode-3.0`.
- Dev only: `insta` (snapshots, `default-features = false`) and `proptest`
  (`default-features = false, features = ["std"]`).
- No `thiserror`, `clap` or `tempfile`: the error enum, argument parsing and temp dir are
  each a few dozen lines.

## Tests
- Unit and property tests for the literal parser (hex, decimal, binary and octal agree;
  Display round trip; overflow rejected) and the width calculator (range length, struct
  tiling, packed array overflow, storage word count).
- `tests/fixtures/<case>/`: `design.sv`, `case.json` (top, `-G` params, defines),
  committed `tree.json`, `tree.meta.json` and `verilator-version` from 5.052, and insta IR
  snapshots in `tests/it/snapshots/`.
- `runner::json_pass_matches_committed_fixtures`: the installed Verilator still produces
  the committed IR.
- `header::storage_matches_generated_headers`: runs `verilator --cc` on every fixture and
  checks each top port's `cpp_member`, `Storage` and width against the generated header.
- `strict::errors_name_path_and_version`: injected unknown fields, nodes, keywords, wrong
  `lsb` and dangling references, with snapshotted messages.
- Regenerate: `pixi run cargo nextest run -p svtypes --run-ignored only regen_fixtures`,
  then review the fixture diff and the snapshot changes.

## Not verified / open
- Verilator other than 5.052 (5.050, 5.051, 5.053 to 5.059 are accepted by version
  string only).
- `VlUnpacked` index mapping (whether element 0 is the left or the low bound).
- Interface arrays, modport task imports, unpacked struct ports, `string`/`real` ports,
  array-valued parameters: rejected with an error, not supported.
- Parameterised interfaces whose typedefs depend on parameters, and typedefs inside
  generate blocks.
- The tests that run Verilator need it on `PATH` (`pixi run`); without it they fail.
