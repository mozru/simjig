# n-sweep notes: first-class parameter sweeps

Supersedes the sweep part of `s3c8-fix.md` (option (c), the README trait pattern). Its
objections to (a) shared types and (b) a generated trait are answered below.

## What it is
- `simjig_build::Sweep::new("sync_fifo").dut(..).dut(..).build()` builds each member as an
  ordinary DUT, then lowers every member again from the IR its build used, and
  `codegen::lower_sweep` decides the sweep module: the items, packages and bundles equal
  in every member (a fixpoint: nothing that names an unshared type is shared), the
  `Ports` trait (a method per `Dut` field every member has with one access; concrete
  types where the members agree, else an associated type per distinct list of
  per-member types, bounded by `simjig::Vector`, `simjig::Value` or nothing), the
  parameters as associated consts, and a bundle trait per interface port whose bundle
  differs.
- `OUT_DIR/simjig/<sweep>.rs` holds those and `pub mod <member> { include!("<member>.rs"); }`
  per member, with the trait impls; each member's file is rewritten to
  `pub use super::{..}` the shared names. `include_dut!(<sweep>)` is unchanged: the sweep
  is one more file in `OUT_DIR/simjig/`.
- `#[simjig::test(duts = [a::Dut, b::Dut])]` on a generic `async fn t<D: s::Ports>(dut: D)`
  expands to `mod t { use super::*; <fn>; #[test] fn a() .. #[test] fn b() .. }`.
- `simjig::{Value, Vector}`: blanket-implemented bound bundles, so the body needs no
  `where` clause.

## Why not simpler
- **No sweep, keep the README pattern (c).** About 40 lines of user trait and macro per
  top, redone by every user, and it cannot share struct types: `small::Hdr` and
  `big::Hdr` stay two Rust types. The brief asked for it gone.
- **No `Sweep` builder: group DUTs of one `.top()` implicitly.** The build script would
  have to rewrite the sweep file after every `Dut::build`, never knowing which DUT is the
  last; an unrelated second build of the same top (another wrapper, other probes) would
  join the sweep and change its trait. An explicit list decides the members once, and
  names the module (`include_dut!(sync_fifo)`).
- **Retype handles instead of sharing types** (keep each member's `Hdr`, convert
  `In<small::Hdr>` to `In<sweep::Hdr>` in the impl). One SV type would be two Rust types
  in one test (`dut.hdr` versus `dut.hdr()`), and the conversion is a new unchecked
  runtime surface. Sharing by `pub use super::X` is plain Rust; its cost is that a
  member's file can only be included through its sweep.
- **A trait per top in the runtime, or generic `Bits<{W}>`.** Stable Rust cannot make a
  handle generic over a width parameter computed from a module parameter
  (`Bits<{$clog2(DEPTH)+1}>`), so associated types are the only stable answer.
- **One associated type per port** (not per distinct per-member type list). Then `wr_data`
  and `rd_data` would be `D::WrData` and `D::RdData`, two types a generic body cannot equate
  (no equality bounds in traits on stable), and a FIFO model could not compare what it
  wrote with what it read. Sharing changes the trait when a later member breaks the
  agreement; that is the nature of the abstraction and shows as a compile error.
- **Name associated types after a parameter** (`D::Width`). Guessing which parameter a
  width follows is a heuristic that breaks on `$clog2(DEPTH)+1`; the first port's name is
  predictable, and each associated type's docs list its ports and per-member types.
- **Bundle traits.** Interface ports whose bundle follows a parameter (`stream_if`
  `WIDTH`) are the common case for interface-ported DUTs; leaving them out of `Ports`
  would leave those DUTs with the old pattern. They reuse the same trait builder.
- **`duts = [..]` in the macro.** Without it the generic body works but each
  configuration needs a two-line `#[simjig::test]` wrapper; the attribute is
  parse-and-repeat, and the body is kept as written in the module, so spans (and failure
  locations, checked: `sync_fifo.rs:623:9` for both configurations) are the user's.
- **`Vector` blanket-implemented with a provided `to_u128`.** Through the words of
  `Packed::Raw`, so it needs no per-type impl in `simjig-bits` (which may not know
  `RawWords`). It panics on a `WideBits` value above 128 bits rather than truncating.

## Contracts that changed
- Generated code: new per-sweep file and member preamble (`docs/notes/codegen.md`);
  `__private` gains `Value` and `Vector`. A DUT built outside a sweep is generated
  byte-for-byte as before (every existing snapshot is unchanged).
- `simjig-build`: `Sweep`, `BuiltSweep`, `codegen::{lower_sweep, emit_sweep,
  emit_sweep_member, LoweredSweep, ..}`; a DUT may not be named like a sweep of the same
  script, and a DUT may be in one sweep only (`script::claim_sweep`).
- `#[simjig::test]`: `duts = [..]`; generic functions allowed only with it; two error
  messages list it.

## Limits (documented, not fixed)
- A method or const of an associated type's bound needs its trait in scope
  (`use simjig::Vector;` for `.to_u128()`, `use simjig::bits::Packed;` for `::WIDTH`,
  the bundle trait for a bundle's signals): Rust does not search a projection's item
  bounds without the import, unlike a type parameter's bounds.
- A member's generated file cannot be `include_dut!`ed on its own (it imports from
  `super`); `cargo simjig show <member>` still prints it.
- DPI-C traits stay per member (`Dut::set_dpi` is per member).
- A field not in every member alike is left out of `Ports` and named in its docs.

## Verification
- `codegen_it::sweep`: decisions on `cases/sweep` (W 8/200, N 2/3: shared enum, struct and
  package; `a`/`b` sharing `A`; `Word` bounded by `Value`; `Lanes` unbounded) and on
  `cases/iface_param` (a bundle trait), snapshots, and the committed generated files
  compiled with one generic body against the stub runtime.
- `it::sweep`: a real two-member build, refusals (two modules, a failed member, name
  clashes both ways, a member in two sweeps, a custom emitter), the stub.
- `examples/lib/tests/sync_fifo.rs`: the raw FIFO's tests run on `fifo_8x4` and
  `fifo_32x16` through the real runtime; the README's snippets are checked to be that
  crate's code.
