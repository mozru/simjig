# n2-fix: review fixes after n1-fix

The reviewer's patch (`cases/sweep_fields`, two failing tests) is adopted. One assertion
was extended: it expected `AIfBeatT`'s equalities to be `[Data = Din]` only, but
`beat_t` also has `tag` (`W+1` bits), whose type differs too, and the fix fixes every
type of the fields trait (`[Data = Din, Tag = Tag]`). Fixing a subset would leave
`beat.tag()` unrelated to the same type reached another way.

## 1. A fields trait's type named `Raw`
`lower::sweep::fields_trait`'s associated-type namespace reserves `VALUE_TYPES` (`Raw`,
the only associated type of `Value`/`Packed`; `Vector` adds none), so a field `raw` of a
differing type gets `Raw2`. Test: `a_fields_traits_types_are_not_named_like_a_supertraits`;
the generated `fields_sweep/fn.rs` compiles with it.

## 2. A fields trait's types are `Ports`'
`Builder::place` replaces `assoc_type` where `Ports` and bundle traits get a signal's
type: one bounded by a fields trait also brings that trait's associated types in,
recursively, and fills `equals` (`type Beat: BeatFields<Data = Self::Din, Tag =
Self::Tag>`, `type Outer: OuterFields<Inner = Self::Inner>`, `type Inner: InnerFields<A
= Self::A>`). `lift` (from `bundle_type`) moves a bundle trait's types into `Ports` and
now remaps their own `equals`, so `Up: BeatIfSinkPorts<Beat = Self::Beat, Data =
Self::Data, Tag = Self::Tag>`.

- Why a fields trait's own types stay its fields' only (`OuterFields::Inner:
  InnerFields`, not `<A = Self::A>`): the reviewer's test expects that, and a body over
  `Ports` sees everything tied through `Ports`. Trade-off: a body generic over
  `OuterFields` alone cannot relate `o.i().a()` to another field of `outer_t` of the
  same type. Revisit if a user writes such a body.
- Visible change: `Ports` and bundle traits gain the types struct fields bring (`D::Tag`,
  `D::Data` in `bt`); existing names are unchanged (dedup by per-member types). Snapshots
  `beat_sweep__bt.rs`, `sweep__sw.rs` reviewed; `fields_sweep__*` new.
- Evidence: `a_fields_traits_types_are_fixed_to_the_ports_traits` (fails without: `left:
  []`), `a_generic_body_compares_struct_fields_with_ports` (compiles `field == port`
  bodies over both sweeps), and `examples/lib` `hop_stage` gains a port `down_data`
  (`= down.beat.data`) compared with `hopped.data()` at run time; the README snippet
  shows it. In `hop_stage` the shared type is `D::DownData` (the flat port comes first,
  so it names the type).

## 3. Toolchain rerun filter
`archive::toolchain_dirs` now keeps only (`toolchain_from`, pure, unit-tested):
- the resource dir: `-print-resource-dir` (Clang) and the parent of
  `-print-file-name=include` (GCC's `lib/gcc/<triple>/<ver>`, holding `include` and
  `include-fixed`; Clang's resource dir again). A relative answer (GCC's `include` when
  it has none) is dropped;
- the search list's directories with a `c++` component (`include/c++/v1`,
  `c++/13`, `<triple>/c++/13`, `c++/13/backward`);
- the SDK: `xcrun --show-sdk-path` (macOS hosts only) and every `-isysroot <path>` in the
  `-v` report (Clang's cc1 line; GCC's quoted `COLLECT_GCC_OPTIONS`).

Dropped: `<prefix>/include` (and the search list's `bin/../include`),
`/usr/local/include`, `/usr/include`. Not added: `--sysroot` (a cross GCC's sysroot is
the SDK analogue, but the brief named `-isysroot` only).

Measured on macOS (conda clang 21, CommandLineTools SDK), `simjig`'s own test build
script, distinct `rerun-if-changed` paths: **66 before, 69 after**. The three new ones
are `.pixi/envs/default/include/{zlib.h, zconf.h, lz4.h}` (`fstcpp_writer.cpp` includes
them), which is the intended effect: a conda zlib upgrade now reruns the script.
`examples/lib`'s script: 43 before, 46 after (the same three).

On Linux, every libc header under `/usr/include` will now be a rerun path (GCC's
`-v` puts `/usr/include` in the list); expect a few dozen more lines there. Not
measured: the Linux VM run is later. The parsing is checked by
`gccs_report_gives_its_own_dirs_and_not_usr_include` on a GCC 13 shaped report.

## 4. Nits
- Blanket methods: `HANDLE_METHODS` and `VALUE_METHODS` add `into`, `from`, `try_into`,
  `try_from`, `to_owned`, `clone_into`, `borrow`, `borrow_mut` (renamed `into_` etc.).
  The doc note reads "a supertrait or a blanket impl has a method". Test:
  `a_method_named_like_a_blanket_impls_gets_an_underscore` (`rw_t.into`, port `from`;
  the generated code is compiled and `rw.into_()`, `dut.from_()` called).
- `.sources` appends, `.param` replaces: documented on both and in the README with the
  closure-argument pattern. A path already added is skipped (`Path` equality, so
  `./a.sv` equals `a.sv` after `absolute`). Test:
  `a_source_added_twice_is_kept_once_in_its_first_place`.
- README first sweep test snippet: `duts = [fifo_8x4::Dut, fifo_32x16::Dut]` and
  `D: Ports`. `examples/lib/tests/sync_fifo.rs` now has `use sync_fifo::*;` and the short
  form throughout (needed for the snippet test, and clippy is clean with
  `unused_qualifications`).
- `u64` into a field: not done. A `From<u64>` bound next to `From<u128>` makes a literal
  argument (`D::Beat::new(0, 0x5a)`) fall back to `i32` and fail (E0277), checked with
  a standalone rustc program. Documented in the README (`u128::from(rng.below(n))`).
- `usize` parameter companions: not done (a negative `int` has no `usize`, and a
  `DEPTH_USIZE` could clash with a parameter of that name); documented
  (`.unsigned_abs()`).

## Not verified
- Linux: rerun count and the real GCC `-v`/`-print-file-name` answers.
- A Homebrew prefix on macOS (only conda measured).
