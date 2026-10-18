# s1c3fix: Stage 1 review cycle 3 fixes

Agent `s1c3fix`, branch `fix/s1c3fix` (from `stage/1`). Facts checked against the installed
Verilator 5.052 (pixi) unless marked 5.053-devel source.

## Probes
### Writable probes of constants (FFI bug)
5.052 registers a `public_flat_rw` parameter or localparam with `varInsert(name, &member,
/*isParam*/ true, ...)`, and the member is `static constexpr`, so its address is in read-only
memory: a deposit was SIGBUS on macOS. Three layers now refuse it:
1. the lowering, from the IR (`Module::variables`, below): a `ProbeAccess::ReadWrite` probe
   of a `parameter` or `localparam` is a `codegen::Error::Config`;
2. the scope-table check after verilating (`syms.rs`): `isParam` and `VLVF_PUB_RW`;
3. create (`find_probe(.., access)`): a write-access probe must be `isPublicRW()` and not
   `isParam()`. Read-only probes of parameters stay allowed.

### Probe mistakes fail the build
Verilator's scope table (`V<prefix>__Syms__Slow.cpp`) lists only *public* variables, and a
scope only when it has one: with a single probe, a mistyped variable leaves no scope at all,
so the table cannot tell a variable typo from an instance typo or a wrong module, and has no
names to suggest. (The brief assumed it could; this is the contradiction found.) So:
- **svtypes IR schema 3** adds `Module::instances` (`Instance { name, module }`, the body's
  `CELL`s) and `Module::variables` (`Variable { name, kind }`, the body's `VAR`s other than
  interface refs, kind `Signal`/`Parameter`/`Localparam`), both skipped in JSON when empty.
  Names only, no types: decoding the types of every internal variable would make designs
  that parse today fail on internal types the IR does not support.
- **the lowering** (`check_probe_path`) walks the path through the instances (from the
  wrapper's `dut` instance for an interface-ported DUT), checks the probe's module against
  the instance's, the variable against the module's variables, and the constant rule, with
  a closest-name hint (`names::closest`, Levenshtein, at most one edit per four chars);
- **after verilating** (`syms.rs`), each probe the shim binds (`Emitted::probes`, filled by
  `codegen_emitter`) is checked in the scope table: scope and variable present, width
  (product of the packed ranges, which follow the unpacked ones in a `VlVarTableEntry`),
  packed, access. This is the only place the width is known at build time.

Table facts relied on: scope rows `{offsetof(<Syms>, <member>), "<scope>", ...}` with the
scope named without the model's instance name; `<member>->varsInsertFromTable(<table>, ..)`
and `<member>->varInsert("<var>", <ptr>, <isParam>, VLVT_*, <flags>, <udims>, <pdims>
,<l>,<r>..)`; a scalar has `pdims` 0; a generate scope is named `g.gb[0].u`.

Not supported (documented in `Dut::probe` and tracecov.md): probe paths through generate
blocks.

## Coverage: covergroup bins
`cov_runs` stays "code coverage counters only"; covergroup bins (registered by `new`, in an
eval, heap-allocated) are in no run, and `cov_write`/`cov_zero` include them. The ABI comment
in `simjig_abi.h` says so. `Runtime::cov_runs` now closes the noting (`m_cov_closed`), so
eval-time registrations are no longer appended forever. Details in tracecov.md.
`crates/simjig/src/model/abi.rs` (not owned here) still says "every coverage counter" on its
`cov_runs` field; it should say "every code coverage counter".

## Build
- **Reruns**: `rerun_directives` drops Verilator dependencies under `<cache>/inputs/` (the
  `.vlt`, wrapper, stored `verilator_input`s). They derive from tracked configuration and
  sources, and the run that stores them writes them after the build script started, which
  Cargo saw as a change on the next build.
- **Model archives** are compiled through `Toolchain::cached_archive` into
  `<cache>/archives/<key>/libsimjig_model_<prefix>_<key16>.a`. The key covers the
  (content-addressed) obj dir path, the contents and paths of the model classes, the shim
  and the recipe's model sources, the defines and the toolchain. Every archive key now also
  hashes the files under `CppRecipe::include_dirs` (domain `simjig-archive-v3`), which
  invalidates existing runtime archives once. The shim's path is part of the key, so a
  model is shared between build-script runs of one crate, not across crates.
- **Unbound interface ports**: Verilator writes no JSON for a design with an unconnected
  interface port, and `--lint-only` (which accepts top interface ports,
  `topIfacesSupported()`) cannot be combined with `--json-only`. The build therefore reads
  the JSON pass's diagnostics: `Unsupported: Interfaced port on top level module` (no
  bindings; the DUT is the top) at the port's declaration, or, under the elaboration top,
  `Interface port 'x' is not connected ...` with the preceding `PINMISSING` note that
  locates the declaration. The declaration line gives `interface.modport`. For that,
  `svtypes::Error::Tool::stderr` now keeps the whole stderr (`Display` shows the last 40
  lines).
- **Errors**: `Error::Command { what, status: Option<i32>, .. }` leads with what failed and
  ends with the command line; the `status` change keeps `Error` within clippy's
  `result_large_err` limit (emitters return `Result<Emitted, Error>`).
- Refused raw arguments: `--prefix`, `-Mdir`/`--Mdir`, `--top-module`/`--top`, `--cc`,
  `--exe`, `--build`.

## Tests (each failed before its fix unless noted)
- cpp: `probe_access_is_checked_at_create`, `cov_noting_stops_after_create`,
  `covergroup_bins_are_written_but_not_in_runs` (documents behaviour; passed before);
- codegen_it: `errors::probe_path_mistakes_name_the_fix`,
  `probe_paths_of_an_interface_ported_dut_are_checked_below_the_dut`;
- it: `probes::{read_write_probe_of_a_localparam_fails_the_build,
  probe_width_mismatch_fails_the_build, probe_path_mistakes_fail_the_build}`,
  `fixture_crate::a_no_op_build_after_new_generated_inputs_is_fresh`,
  `ifaces::{no_bindings_lists_every_interface_port, a_partial_binding_lists_the_unbound_ports}`,
  `recipe::editing_one_dut_recompiles_only_its_model`, `errors::verilator_errors_name_the_command_and_file`;
- svtypes: `runner::modules_record_instances_and_variables`,
  `runner::unsupported_ports_lead_with_the_port_and_its_location`, `cli::help_prints_usage`;
- unit: `syms::tests::*`, `names::closest_suggests_likely_typos_only`,
  `error::command_failures_lead_with_what_failed_and_end_with_the_command`, the raw-argument
  and rerun tests in `dut.rs`, the doctor's `verilator_version`.

## Not verified
- Linux (all runs on macOS arm64).
- Split `__Syms` sources (`--output-split`): every `<prefix>__Syms*.cpp` is read, but no
  design here produced more than one.
- A probe of a variable with more than three dimensions (the table's range array then has
  more slots; the parser reads whatever numbers the row has, and skips the width check when
  it cannot compute one).
- UBSan on the toggle helper (documented from the source, not rerun).
