# codegen notes

Agent `codegen`, branch `w2/codegen`: `crates/simjig-build/src/codegen/**` and
`crates/simjig-build/tests/codegen_it/`. Checked against Verilator 5.052 (pixi).

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
(index = slot; `SignalTarget::{Port { cpp_member }, Probe { module, var, root_member }}`),
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
passes silently, hence `-Werror-USERFATAL` in the flags.

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

- `trait Value` and `trait Vector` (the bounds of a sweep's associated types; see
  `simjig::Value`), with blanket implementations for every type that has their bounds.

Generated per model: `Dut` (fields, `IR_HASH: u64`, `SIGNALS: &'static [SignalMeta]`,
top-module params as consts, `#[doc(hidden)] fn __bind(&mut impl Binder) -> Self`) and per
bundle `__bind(&mut impl Binder, &'static [SignalMeta])`.

Generated per sweep (`lower_sweep`, `emit_sweep`): the shared items, `trait Ports` and any
bundle traits, one `pub mod <member> { include!("<member>.rs"); }` per member, and the
trait impls. A member's file starts with `pub use super::{..}` of the shared names.

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
- **`ir_hash`**: FNV-1a 64 over the IR's JSON and the configuration's `Debug` form. The
  Rust `Dut::IR_HASH` and the shim's `kIrHash` are the same value.
- **Shim**: `Traits` in an anonymous namespace, `SIMJIG_SIGNAL` per slot (so the
  `static_assert`s on member type and size apply), probes through
  `m.rootp-><top>__DOT__<inst>__DOT__<var>` (Verilator's `encodeName`: `__` becomes
  `___05F`), and the `___024root.h` include only when there are probes.
- **Generated-code lint posture**: compiles warning-free under the workspace lints
  (pedantic), except `dead_code`/`unreachable_pub` in the test crate, which uses only part
  of each API. Long literals are digit-grouped for `unreadable_literal`.
- **Probes and the IR.** The IR has no internal variables and no instance tree, so a
  probe states its declaring module (for the `.vlt` `-module`, which matches by module
  name) and its type. This deviates from the brief's `(hier_path, Access)`.
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
- Verilator name hashing (`VName::hashedName` for very long names) is not reproduced in
  probe member names.
- Only osx-arm64 was run.
