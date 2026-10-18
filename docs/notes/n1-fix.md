# n1-fix: review fixes after stage N

Each finding of the stage-N review, what changed and why not something simpler.

## 1. The sweep fixpoint is tested
`codegen_it::sweep::a_struct_reaching_an_unshared_type_through_another_is_not_shared`
(reviewer patch 01, on `cases/sweep_nested`). Mutants of `Shared::find` checked by hand:
one pass only (no loop) and `any` for `all` in the stop test both fail it now.

## 2. Models of one design share their initial-value seed
`harness::multi::model_options`: model `i` of a tuple DUT gets `Seed::init_seed(j)`, `j`
the first model with the same `DutModel::design()`. Model 0 and models of other designs
are unchanged, so single-DUT runs, `summary.json`'s `init_seed` and `(inv, flop)` pairs
draw what they drew before.

- `DutModel::design() -> Option<&'static str>` is new (default `None`); generated `Dut`s
  return `ModelMeta::top()`, the first segment of their signals' names (the top module,
  or `simjig_wrap_<dut>`), which needs no change to generated code.
- Why not by `TypeId` (two instances of one build only): the flagship lockstep case is
  two builds of one top module (old and new RTL, two prefixes); `VL_SCOPED_RAND_RESET`
  hashes the seed with the scope and variable names, so one seed makes their like-named
  registers start alike.
- Why not one seed for every model: `inv.q` and `flop.q` would start equal because
  their names are, a correlation n-xinit avoided on purpose.
- Test: `model_tests::init::the_models_of_one_design_start_alike` (patch 02 merged into
  the existing two-instance test, which asserted the opposite), and a unit test of the
  stream choice. Fails without the change (a cycle-0 divergence on `q` and `r`).

## 3 and 4. Sweep ergonomics
Checked against the reviewer's shape (`cases/sweep_beat`: `beat_if` carrying
`beat_t {data[W], tag[T]}` on a sink and a source port). Before: `UpBeat` in two bundle
traits, two unrelated types, `D::Up::UpBeat` does not resolve (E0223), no field access.

- (b) The struct is named `Beat`: the wrapper spells an interface-local aggregate
  inline, so its IR type was anonymous and named after the first flat port. The port's
  type-name candidates now start with the interface signal's typedef (`lower::roots`,
  `iface_typedef`). Also for single DUTs; no existing snapshot changed.
- (a) Each bundle trait's associated types are `Ports`' too, fixed by the bound:
  `type Up: BeatIfSinkPorts<Beat = Self::Beat>`. Rust normalises
  `<D::Up as BeatIfSinkPorts>::Beat` to `D::Beat` through the item bound (checked by the
  compile test). Why not drop the bundle traits' own types: the bundle trait is also
  used on its own (`fn f<B: BeatIfSinkPorts>(b: B)`), and an equality bound is the
  only stable way to tie two traits' associated types.
- 3. `<Name>Fields: Value` per struct associated type, named from the assoc type (the
  typedef): getters, `new(field, ..)`, `with_<field>`, field types concrete or associated
  as for ports (nested structs get their own fields trait). Arguments are
  `impl Into<field>`, so literals infer (`D::Beat::new(0x5a, 1)`). Why a trait per
  struct and not a generic `Fields` trait: field names are the API, and stable Rust has
  no way to name fields generically. Methods clashing with `Value`'s supertraits, `new`
  or another field's setter get `_` appended (`eq_`, `with_x_`).
- `Value::from_u128`/`to_u128`: `to_u128` moved from `Vector` to `Value` (its
  supertrait), `from_u128` added; with both traits in scope only one method exists, so
  no ambiguity. Unreleased API, so no compatibility shim.
- (c) No turbofish: `D::Beat` is a plain path and `set(D::Beat::new(..))` infers.
- (d) The sweep module `pub use`s `::simjig::__private::{Value, Vector, bits::Packed}`
  (skipping a name a shared type takes); `use <sweep>::*;` is a generic body's one
  import. A future `simjig::prelude` re-exporting the same traits stays compatible: two
  glob imports of one item do not conflict.
- 7c. `Ports`/bundle methods named `clone`, `clone_from` or `fmt` become `clone_` etc.
  (`TraitMethod::field` keeps the field), with a doc note.
- End to end: `examples/lib` `hop_stage` (a sweep over `tagged_if`'s `beat_t`), the
  README before/after, its snippet compile-checked by `the_readme_sweep_is_this_crate_code`.

## 5. Duplicate builder settings
`.param` replaces an existing name's value in place (last wins; the prefix equals the
once-set configuration's). A second `bind_iface*` of a port fails `DutConfig::validate`
naming both calls. A duplicated name inside one `bind_iface_with_params` list is not
handled (not reported).

## 6. Build-error noise
- (a) `archive::toolchain_dirs`: the compiler's `#include <...>` search list (`-E -x c++
  -v -` with the tool's arguments minus include-path flags, once per script and
  compiler) plus `<compiler prefix>/include`, each as named and canonical, compared
  lexically (`bin/../include`). `simjig`'s own test build script went from 1,268 distinct
  `rerun-if-changed` paths to 66; the rebuild fixture's prints 77, 69 of them Verilator's
  include directory and executable, which stay (keyed by the Verilator version only, not
  its identity; left as is). Risk named: a compiler installed inside a project tree
  (`<proj>/tools/bin/cc`) would hide edits to `<proj>/tools/include` from reruns (the
  archive keys still see them on the next build).
- (b) `pipeline::blame_param`: the first error's line, read from the file, at a
  `.NAME(` of a generated top or at a `-G` parameter's declaration, names the call.
  Heuristic by location; an error elsewhere is left as it was.
- (c) The hint starts with ``DUT `<name>`: ``.

## Not verified
- Linux: the toolchain filter's `-v` parsing was checked only on macOS (Apple SDK, conda
  clang 21); the unit test feeds a GCC-shaped report.
- The thread_rng syncs remain untested (documented as defensive).
- Android and compiler-rt linking (no toolchain here).
