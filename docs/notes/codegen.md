# codegen notes

Covers `crates/simjig-build/src/codegen/**` and `crates/simjig-build/tests/codegen_it/`.
Checked against Verilator 5.052 (pixi).

## Shape
One lowering feeds every emitter (DECISION.md table 2, row 5). The module is pure: IR in,
text out. Everything is under `simjig_build::codegen` (the module is `pub`, because the
integration tests and the builder both need it).

| Item | What |
|---|---|
| `lower(&Design, &CodegenConfig) -> Result<Lowered, Error>` | The only place that decides names, slots, storage and access. |
| `lower_wrapper(&Design, &IfaceTop) -> Result<Wrapper, Error>` | The wrapper plan, from the DUT module's IR entry (works on the elaboration-top IR and on the wrapper-top IR, with identical results). |
| `emit_rust(&Lowered) -> String` | Types, package consts, bundles, `Dut`. |
| `emit_vlt(&Lowered) -> String` | `public_flat_rd`/`_rw` for probes only. |
| `emit_layout_check(&Lowered) -> String`, `LAYOUT_CHECK_FLAGS` | Module `Lowered.layout.module` (`simjig_layout_<top>`). |
| `emit_wrapper(&Wrapper) -> String`, `emit_elab_top(&IfaceTop) -> String` | Modules `wrapper_module(dut)` (`simjig_wrap_<dut>`) and `elab_module(dut)` (`simjig_elab_<dut>`). |
| `emit_shim(&Lowered, &ShimOptions { prefix, timing }) -> Result<String, Error>` | `<prefix>_shim.cpp` for `cpp/simjig_model.h`. |

`CodegenConfig { probes: Vec<Probe>, iface_top: Option<IfaceTop> }`;
`Probe { path, module, ty: ProbeType::{Named(String), Bits { width }}, access: ProbeAccess::{Read, ReadWrite} }`;
`IfaceTop { dut, params, bindings: Vec<IfaceBinding { port, interface, params, modport }> }`.

`Lowered` is plain data with public fields: `top`, `producer`, `ir_hash`, `items: Vec<Item>`
(`ItemKind::{Alias, Struct, Union, Enum}`), `packages`, `dut_params`, `signals: Vec<Signal>`
(index = slot; `SignalTarget::{Port { cpp_member }, Probe { module, var, scope }}`),
`fields: Vec<DutField>`, `bundles`, `layout`, `wrapper`.

### Build flow (for buildinfra)
Plain DUT: JSON pass with the DUT as top, `lower`, emit everything.

Interface-ported DUT (Verilator rejects interface ports on the top, and `--json-only` too):
1. write `emit_elab_top(&iface)`, JSON pass with top `elab_module(dut)`;
2. `lower_wrapper(&design1, &iface)`, write `emit_wrapper(..)`;
3. JSON pass with top `wrapper_module(dut)` (sources + wrapper), `lower(&design2, &config)`;
4. build the model with the wrapper as top. DUT parameters go in `IfaceTop.params`, not
   `-G` (which would reach the wrapper).

Layout check: `verilator <model flags> LAYOUT_CHECK_FLAGS --top-module <layout.module>
<sources> <layout file>`, without `-G` (every type is spelled with elaborated widths).
**Elaboration-time `$fatal` is the `USERFATAL` warning in 5.052**: under `-Wno-fatal` it
passes silently, hence `-Werror-USERFATAL` in the flags. Order matters: a later
`-Wwarn-USERFATAL` downgrades an earlier `-Werror-USERFATAL`, and `-Wno-USERFATAL`
(anywhere) disables the warning until a later `-Wwarn-USERFATAL` (`-Werror-` alone does not
re-enable it). So the check's flags end with `-Wwarn-USERFATAL --lint-only
-Werror-USERFATAL`, after the user's arguments.

## The `__private` contract
Generated Rust names only `::core`, `::std::vec::Vec` and these runtime items. The stub in
`tests/codegen_it/stub.rs` is the reference.

- `bits`: a re-export of `simjig_bits` (`Bit`, `Bits`, `WideBits`, `Packed`).
- Handles `In<T>`, `Out<T>`, `InOut<T>`, `PublicRd<T>`, `PublicRw<T>`. `T` is a
  `Packed` type, or `[T; N]` (nested) for unpacked-array ports.
- `trait Binder { fn input<T: 'static>(&mut self, meta: &'static SignalMeta) -> In<T>;`
  and `output`, `inout`, `public_rd`, `public_rw` likewise `}`. Called as
  `Binder::input(binder, &Self::SIGNALS[i])` in field order (= slot order).
- `struct SignalMeta { name: &'static str, slot: u32, width: u32, storage: Storage, access: Access }`
  built with a struct literal in a `const`.
- `enum Storage { CData, SData, IData, QData, VlWide { words: u32 }, VlUnpacked { elem: &'static Storage, len: u32 } }`.
- `enum Access { In, Out, InOut, PublicRd, PublicRw }`.
- `InvalidEnum::new(type_name: &'static str, width: u32, value: u128) -> InvalidEnum`.
- `FieldDiff::new(path: &str, left: &dyn Debug, right: &dyn Debug) -> FieldDiff` and
  `FieldDiff::within(self, parent: &str) -> FieldDiff` (prefixes `parent.`).
- `trait RawWords: Copy { fn to_words(self, out: &mut [u32]); fn from_words(words: &[u32]) -> Self; }`
  for `u8`, `u16`, `u32`, `u64` (two words) and `[u32; N]`. `from_words` must ignore bits
  beyond the storage.
- `fn pack_field<T: Packed>(dst: &mut [u32], lsb: u32, value: T) where T::Raw: RawWords`
  and `fn unpack_field<T: Packed>(src: &[u32], lsb: u32) -> T where T::Raw: RawWords`.

- `trait Value` (with provided `from_u128`, `to_u128`) and `trait Vector` (the bounds of a
  sweep's associated types; see `simjig::Value`), with blanket implementations for every
  type that has their bounds; the sweep module re-exports them and `bits::Packed`.

Generated per model: `Dut` (fields, `IR_HASH: u64`, `SIGNALS: &'static [SignalMeta]`,
top-module params as consts, `#[doc(hidden)] fn __bind(&mut impl Binder) -> Self`) and per
bundle `__bind(&mut impl Binder, &'static [SignalMeta])`.

Generated per sweep (`lower_sweep`, `emit_sweep`): `pub use ::simjig::__private::{Value,
Vector, bits::Packed};`, the shared items, `trait Ports`, any bundle traits (whose
associated types `Ports` repeats and fixes: `type Up: BeatIfSinkPorts<Beat = Self::Beat>`)
and fields traits (`trait BeatFields: Value`: getters, `new`, `with_<field>`), one
`pub mod <member> { include!("<member>.rs"); }` per member, and the trait impls. A
member's file starts with `pub use super::{..}` of the shared names.

## Decisions
- **Names.** snake fields/modules, CamelCase types, SCREAMING consts; raw identifiers for
  keywords (`r#loop`), `_` suffix for `self`/`Self`/`super`/`crate`. Typedef names drop
  one `_t`/`_e`/`_s`/`_u`. On a collision (with each other, `Dut`, or a prelude name) every
  colliding type falls back together: unstripped, then scope-qualified, then numbered
  (`part_t`/`part_u` become `PartT`/`PartU`; `vec_t` becomes `VecT` because `Vec` is
  reserved). Anonymous types are named after their first use (`top.in_anon` is `InAnon`).
  Fields, union views, enum consts collide with `_2` suffixes in declaration order.
- **Enums** (DECISION table 2, row 7): newtype over `Bit`/`Bits<N>`, one const per variant
  with a shared `PREFIX_` stripped (`OP_ADD` is `Op::ADD`), companion `enum OpVariant`,
  `variant() -> Result<OpVariant, InvalidEnum>`, `From<OpVariant>`, `Debug` with the SV name
  or `Op(2'h3)`. `variant` is an `if` chain: over `Bit` two variants are exhaustive and a
  `match` would have an unreachable arm. Enums above 128 bits are unsupported.
- **Structs**: `Packed` through `pack_field`/`unpack_field` at the IR `lsb`s, plus a
  compile-time `assert!` that each field type's `WIDTH` is the IR width; derived `Debug`;
  `diff(&self, &Self) -> Vec<FieldDiff>` recursing into struct fields with SV paths.
- **Unions**: newtype over the bits; `view()` / `from_view(v)` per member; reserved
  `bits`, `to_bits`, `raw`, `to_raw` (a member named `raw` becomes `raw_2`).
- **Value types**: packed arrays become `Bits` of their total width; signedness is dropped
  (no `SBits` yet); integral params are the smallest primitive (`bool` for one unsigned bit),
  `WideBits` above 128 bits, enum-typed ones the generated enum.
- **Access**: `input` is `In` (testbench drives), `output` is `Out`, `inout` is `InOut` (a
  fifth kind, needed by `scalar_vector_wide`). Bundle members keep the modport direction,
  which the IR already gives from the DUT's side, so no flip is needed: the DUT's `input`
  is the testbench's write.
- **Wrapper**: flat ports `<port>_<signal>` (`in_bus_valid`; the hand-written reference used
  `in_valid`, which is not derivable). Interface ports of the interface (`bus_if(input clk)`)
  are connected at the instance and also exposed as `In` bundle members. Data ports first,
  then each interface in declaration order, members in modport order. Anonymous and
  module-local enums cannot be spelled outside the DUT, so they are an error in the wrapper.
- **Layout check**: per type `$bits`, per member a ones-mask at the IR range read back as
  all ones and its complement as zero, and nameable enum constants. Anonymous and
  module-local structs are spelled structurally; unnameable enums are spelled as their
  base type (no constant redeclaration) and not checked on their own.
- **`ir_hash`**: the first 64 bits of `hash::ir_hash` (SHA-256 over the IR and the
  configuration, field by field). The Rust `Dut::IR_HASH` and the shim's `kIrHash` are the
  same value.
- **Shim**: `Traits` in an anonymous namespace, `SIMJIG_SIGNAL` per port slot (so the
  `static_assert`s on member type and size apply), and `SIMJIG_PROBE` per probe, found at
  create through the scope table (see "Probes" below).
- **Generated-code lint posture**: compiles warning-free under the workspace lints
  (pedantic), except `dead_code`/`unreachable_pub` in the test crate, which uses only part
  of each API. Long literals are digit-grouped for `unreadable_literal`.
- **Probes and the IR.** The IR has no types for internal variables, so a probe states its
  declaring module (for the `.vlt` `-module`, which matches by module name) and its type;
  the path walk checks both against the IR's instances and variable names.
- Shared-file changes: `simjig-build` depends on `svtypes` and `serde_json` (for the hash),
  dev-depends on `simjig-bits`, `insta`, `proptest`; `deny.toml` gained
  `allow-wildcard-paths = true` (first internal path dependency; all crates are
  `publish = false`).

## Tests (`crates/simjig-build/tests/codegen_it`)
- `snapshots`: Rust, `.vlt`, layout, wrapper and shim for the 10 svtypes fixtures,
  `struct_pipe`, `iface_top` (through the generated wrapper), `producer` (the fixture's
  parameterised interface with a `clk` port) and `cases/wide` (307- and 107-bit structs with
  word-crossing fields, a union over a struct, Rust keywords, three probes).
  `committed_generated_rust_is_current` keeps `generated/*.rs` in step
  (`SIMJIG_BLESS=1` rewrites them).
- `compile`: `generated/*.rs` compiled against the stub; proptests that, from random
  storage with dirty upper bits, every field holds exactly the bits at its IR offset and
  `to_raw` returns the cleaned storage (8 structs, including the wide ones); round trips;
  enums with unknown values; union views; `diff`; consts; `__bind` slot order; `IR_HASH`.
- `errors`: snapshot of every configuration error message.
- `verilator`: layout checks of every case pass and are `-Wall` clean; three corruptions
  (offset, width, enum value) each fail with their message; wrappers are `-Wall` clean and
  the plan from the wrapper-top IR reproduces the written file; the `wide` shim builds
  with `verilator --cc --exe --build` against `cpp/`, and a C++ driver checks the vtable
  (ABI version, size, prefix, ir_hash), the signal table row by row against `Lowered`,
  a port path, and a `public_flat_rw` probe driving logic read back through a
  `public_flat_rd` probe.

## Not verified / open
- `kTiming = true` shims were not built end to end (only `--no-timing`).
- The Rust `ModelVtable::check` was not called: `simjig-build` must not depend on `simjig`,
  so the C++ driver checks the same fields instead.
- Unpacked-array ports: the Rust side types them `[T; N]`, but the shim rejects them, since
  `simjig_signal` describes one packed value and `SIMJIG_SIGNAL` takes no `VlUnpacked`. The
  `VlUnpacked` index order is unverified (svtypes notes too).
- Generic interface ports are supported only with an explicit modport in the binding.
- Interface arrays, `inout` modport signals, enums over 128 bits, unpacked struct ports:
  errors.
- Linux: the gate passes on linux-aarch64 (buildinfra.md, "Linux"); linux-64 was not run.
- A probe of a variable with more than three dimensions: the scope table's range array then
  has more slots; the parser reads whatever numbers the row has, and skips the width check
  when it cannot compute one. Not tried.
- `--hierarchical` with a probe outside the hierarchical block, and probes in interface
  instances.

## Probes
Probes are not C++ members (see the shim's module docs): the generated shim binds each one
at create with `simjig::find_probe`, `ctx.scopeFind("<model name>.<scope>")->varFind("<var>")
->datap()`. Binding `rootp-><top>__DOT__<inst>__DOT__<var>` worked only when every module
was inlined into the root; 5.052 keeps a module apart when it is large and used several
times, declares a class, or carries `/*verilator no_inline_module*/` (all three reproduced,
and the old shim failed to compile).

Verilator 5.052 facts the lowering relies on (`V<prefix>__Syms__Slow.cpp`):
- scope and variable names are the plain identifiers, not C++ encodings: a scope `u__0` is
  registered as `"top.u__0"` and a variable `my__x` as `"my__x"`;
- the scope prefix is the model's instance name (`opts.name`, default `TOP`);
- an interface-ported DUT's probes live under the wrapper's instance:
  `TOP.simjig_wrap_<dut>.dut.<path>`. Users write the path from the DUT
  (`r_dut.u_core.acc`); the lowering rewrites it under `wrapper_module(dut)` and
  `DUT_INSTANCE`, rejects the wrapper spelling, and keeps the user's path as the signal
  name;
- the table lists only *public* variables, and a scope only when it has one. With a single
  probe, a mistyped variable leaves no scope at all, so the table cannot tell a variable
  typo from an instance typo or a wrong module, and has no names to suggest. Hence the IR's
  `Module::instances` and `Module::variables` (names and kinds only: decoding the type of
  every internal variable would make designs that parse today fail on internal types the
  IR does not support), which the lowering's path walk uses;
- a `public_flat_rw` parameter or localparam is registered with `isParam` true, and its
  member is `static constexpr`, in read-only memory: a deposit was SIGBUS on macOS. A
  writable probe of a constant is refused at every layer; read-only ones are allowed;
- `real r;` and `string s;` are `VLVT_REAL`/`VLVT_STRING` with 0 packed dimensions, so they
  read as 1 bit wide: the type is checked before the width;
- with `--output-split 1 --output-split-cfuncs 1`, the tables and scope rows are in
  `__Syms__Slow.cpp` and the `varInsert` calls in `__Syms__ctor__<n>__Slow.cpp`, so
  `syms.rs` resolves calls only after reading every `__Syms*` file;
- a probe inside a hierarchical block (`hier_block`) cannot be served: the block is a
  separate model, and the top's scope exists but not the variable. The pure lowering cannot
  see the metacomment, so the scope-table check reports it, as a missing variable with a
  `hier_block` hint.

Generate blocks. 5.052 dumps the JSON after V3Width and before V3Begin, when generate
blocks are already elaborated `GENBLOCK` nodes:
- a named block keeps its label (`g_x`); a false generate-if is gone;
- an unnamed block is `genblk<N>` with `unnamed: true`, N counting the module's generate
  constructs (IEEE 1800-2023 §27.6): the second `if` of a module is `genblk2`;
- a generate-for is unrolled into one block per iteration named `label[i]`, preceded by an
  `implied: true` block `label` that holds only the `GENVAR` (skipped in the IR);
- an instance array is one `CELL` with a `rangep`.

The scope table names these scopes the same way (`pipe.g_x.u_x`, `pipe.genblk2.u_anon`,
`pipe.g[0].u`; a loop body's variable is in scope `pipe.g[0]`), so the IR records them
(`Module::generate_blocks`, `Instance::array`) and the path walk accepts `ident[digits]`.
Verilator does register `arr[0]` scopes for an instance array, but those are refused.

## Interface-ported DUTs
- `Lowered.dut_params` comes from the DUT module (`lower/wrapper.rs::dut_module`), not the
  wrapper top, which has none.
- An interface-local aggregate is spelled inline in the wrapper, so its IR type is
  anonymous. The port's type-name candidates start with the interface signal's typedef
  (`lower::roots`, `iface_typedef`), so it is named `Beat`, not after the first flat port.

### `timeunit` in generated tops (5.052, `--lint-only`, a wrapper around an interface-ported DUT)
| User units | Wrapper | Result |
|---|---|---|
| all `timeunit 1ns` | none | `TIMESCALEMOD` on the wrapper |
| all `timeunit 1ns` | `timeunit 1s; 1s` | passes |
| none | `timeunit 1s; 1s` | `TIMESCALEMOD` on the user's DUT |
| none | none | passes |
| none, `--timescale` | `timeunit 1s; 1s` | passes |
| all `timeunit 1ns` | `lint_off TIMESCALEMOD` | passes; the wrapper's unit is `1ns`, precision `1ps` |
| mixed | `lint_off TIMESCALEMOD` | `TIMESCALEMOD` on the user's unit |

Emitting a `timeunit` only when the user's units have one would need the per-module unit in
the IR (a schema change), so the generated tops waive the warning instead. An untimed
package is flagged ("Timescale missing on this module"); an untimed interface used by a
timed module is not.

### Interface parameters
A bound interface's parameter that the DUT also declares follows a `.param` unless the
binding sets it (`pipeline::elaborate`). Why not refuse a binding value that differs from
the DUT's: IEEE 1800-2017 §25.5 interface port headers carry no parameters, so the name tie
is a heuristic, and a module may read a wider interface through its port (`out.W`), which is
correct SystemVerilog (regression test
`errors::an_explicit_interface_param_is_not_refused_by_name`). When neither side is set and
the defaults differ, the build warns. The warning is computed from the IR the emitter sees
(the wrapper's, which holds the DUT and each bound interface), so a front-cache hit warns
too. Only representable parameters are compared, by their displayed value.

A width error from such a pair leads with a `hint:` naming both defaults. The failed JSON
pass wrote no IR, so the mismatches come from rerunning the elaboration top with
`-Wwarn-WIDTH*` appended (a later `-Wwarn-` beats an earlier `-Werror-`).

### Diagnostics through the generated tops (5.052)
- Verilator writes no JSON for a design with an unconnected interface port, and
  `--lint-only` (which accepts top interface ports) cannot be combined with `--json-only`.
  So `iface_diag.rs` reads the JSON pass's stderr: `Unsupported: Interfaced port on top
  level module` at the port's declaration (no bindings), or, under the elaboration top,
  `Interface port 'x' is not connected ...` with the preceding `PINMISSING` note that
  locates the declaration. The declaration line gives `interface.modport`.
- Binding mistakes: `%Error-PINNOTFOUND: <elab>:L:C: Pin not found: 'in_bs'` (the hint
  comes from the `PINMISSING` pins of the same run), `Port 'in_bus' expects 'bus_if'
  interface but pin connects 'other_if' interface`, `Cannot find file containing
  interface: 'nope_if'`.
- The elaboration top triggers `PINMISSING` (one per open data port) and, in the user's
  interface file, `UNDRIVEN`. Waiving `PINMISSING` would break the unbound-port diagnosis,
  which reads that very warning, and a `lint_off` cannot reach the `UNDRIVEN` in the user's
  file. So the JSON pass's error drops the warnings located in the generated tops
  (`error::without_warnings_in`) instead. The wrapper triggers no warning under `-Wall` in
  the shapes tried, and the model build still reports any it does: that would be simjig's
  bug.

## Parameter sweeps
`simjig_build::Sweep` builds each member as an ordinary DUT and then generates one Rust
module for all of them (`sweep.rs`, `codegen::lower_sweep`; their module docs describe the
sharing rules and the traits). Why not simpler:
- **No sweep: a user trait and a macro per top.** About 40 lines per top, redone by every
  user, and it cannot share struct types: `small::Hdr` and `big::Hdr` stay two Rust types.
- **Group the DUTs of one `.top()` implicitly.** The build script would rewrite the sweep
  file after every `Dut::build`, never knowing which DUT is the last, and an unrelated
  second build of the same top (another wrapper, other probes) would join the sweep and
  change its trait. An explicit list decides the members once and names the module.
- **Retype handles instead of sharing types** (convert `In<small::Hdr>` to
  `In<sweep::Hdr>`). One SV type would be two Rust types in one test, and the conversion is
  a new unchecked runtime surface. Sharing by `pub use super::X` is plain Rust; its cost is
  that a member's file can only be included through its sweep.
- **A trait per top in the runtime, or generic `Bits<{W}>`.** Stable Rust cannot make a
  handle generic over a width computed from a parameter (`$clog2(DEPTH)+1`), so associated
  types are the only stable answer. The trait's shape changes with every parameter that
  reaches a width; that is the nature of the abstraction and shows as a compile error.
- **One associated type per port.** `wr_data` and `rd_data` would be two types a generic
  body cannot equate (no equality bounds in traits on stable), so a FIFO model could not
  compare what it wrote with what it read. Hence one per distinct list of per-member types.
- **Name associated types after a parameter** (`D::Width`). Guessing which parameter a
  width follows breaks on `$clog2(DEPTH)+1`; a type or port name is predictable.
- **Drop the bundle traits' own associated types.** A bundle trait is also used on its own
  (`fn f<B: BeatIfSinkPorts>(b: B)`), and an equality bound
  (`type Up: BeatIfSinkPorts<Beat = Self::Beat>`) is the only stable way to tie two traits'
  associated types. Rust normalises `<D::Up as BeatIfSinkPorts>::Beat` to `D::Beat` through
  the item bound.
- **A generic `Fields` trait instead of one per struct.** Field names are the API, and
  stable Rust cannot name fields generically. A fields trait's own types are its fields'
  only (`OuterFields::Inner: InnerFields`, not tied further); trade-off: a body generic over
  `OuterFields` alone cannot relate `o.i().a()` to another field of the same type. Revisit
  if a user writes such a body.
- **`duts = [..]` in `#[simjig::test]`.** Without it each configuration needs a two-line
  wrapper test. The attribute is parse-and-repeat and keeps the body as written, so spans
  and failure locations are the user's.
- **`Vector` blanket-implemented, with `to_u128`/`from_u128` on `Value`.** Through the words
  of `Packed::Raw`, so `simjig-bits` needs no per-type impl. It panics on a `WideBits` value
  above 128 bits rather than truncating.
- **Not done: `u64` arguments to fields-trait setters.** A `From<u64>` bound next to
  `From<u128>` makes a literal argument fall back to `i32` and fail (E0277). Users write
  `u128::from(x)`.
- **Not done: `usize` companions of parameters.** A negative `int` has no `usize`, and a
  `DEPTH_USIZE` could clash with a parameter of that name; users write `.unsigned_abs()`.

Limits:
- A method or const of an associated type's bound needs its trait in scope
  (`use simjig::Vector;` for `.to_u128()`, `use simjig::bits::Packed;` for `::WIDTH`, the
  bundle trait for a bundle's signals): Rust does not search a projection's item bounds
  without the import. The sweep module re-exports `Value`, `Vector` and `Packed`, so
  `use <sweep>::*;` is a generic body's one import.
- A member's generated file cannot be `include_dut!`ed on its own (it imports from
  `super`); `cargo simjig show <member>` still prints it.
- DPI-C traits stay per member (`Dut::set_dpi` is per member).
- A field not in every member alike is left out of `Ports` and named in its docs.

## Known limitations
- X/Z constants (`ir::Unrepresentable`) are left out of the generated package without a
  doc line saying so.
- The path-walk error for a missing instance repeats the name when the scope and its module
  are named alike: "no instance `x` in `itop` (a `itop`)".
