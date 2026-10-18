# s1fix-codegen notes

Agent `s1fix-codegen`, branch `fix/s1fix-codegen` (from `stage/1`, with `w3/tracecov`
merged at the director's request). Checked against Verilator 5.052 (pixi).

## Layout check: local typedefs
Verilator 5.052 crashes (`V3LinkParse.cpp:681: Implicit enum/struct type created under
unexpected node type`) when an aggregate type written inline in an expression nests another
aggregate. `V3LinkParse::visit(AstDefImplicitDType*)` walks up to the nearest `Var`,
`Typedef`, `NodeFTask` or `ParamTypeDType`; `$bits(struct packed {...})` has none. A plain
`$bits(struct packed { logic a; })` also crashed in a one-file repro but not after a package,
so the rule is not just "nested": naming every aggregate is the only safe form.

The layout module now starts with `typedef <decl> simjig_t<type id>;` for every spelled-out
aggregate (dependencies first, one per IR type), and every check uses those names.
`LayoutCheck.sv_name` (the IR use site, e.g. `top.in_anon`) is what messages print.
The wrapper still spells aggregates inline: they appear only in port declarations (a `Var`).

## Interface-ported DUTs
- `Lowered.dut_params` comes from the DUT module (`lower/wrapper.rs::dut_module`), not the
  wrapper top, which has none.
- Probe paths start with the DUT (`r_dut.u_core.acc`). The lowering rewrites them under
  `wrapper_module(dut)` and `DUT_INSTANCE` (`"dut"`, now one constant used by both SV
  emitters) to find `simjig_wrap_r_dut__DOT__dut__DOT__u_core__DOT__acc`. The wrapper
  spelling is rejected. `Signal.name` of a probe is the user's path. Port signal names
  still carry the wrapper name (`simjig_wrap_<dut>.<port>`); not changed here.

## Probes and non-inlined builds (rejected for now)
Probes are root members, `rootp-><top>__DOT__<inst>__DOT__<var>`, which exist only when
every module is inlined into the root. With `-fno-inline`, Verilator 5.052 keeps each module
in its own class (`Vprefix_acc`) reached through `vlSymsp->TOP__wide_top__u_acc`, with plain
member names (`sum`; even the top's own `knob`), so the shim does not compile.

`lower/inlining.rs` rejects, when there are probes:
- `-fno-inline`/`-O0` unless a later `-finline`/`-O1..3` (last wins; one or two dashes);
- `--hierarchical` unless a later `--no-hierarchical`;
- control-file lines `no_inline -module ..` and `hier_block -module ..` (not the `-task`/
  `-function` forms).

`CodegenConfig` gained `verilator_args` and `control_files` (filled by
`DutConfig::codegen_config` from `verilator_args` and the `.vlt` entries of `generated`;
a one-function change in `dut.rs`, outside my ownership). The IR hash keeps hashing only
`probes` and `iface_top` under the old `Debug` text, so no existing hash changed.

Not detected: `/*verilator no_inline_module*/` and `/*verilator hier_block*/` metacomments
in sources, `-f`/`-F` option files, and `.vlt` files passed as plain sources. The pure
lowering reads no files.

**Proper fix:** resolve probes at create time through Verilator's scope table instead of
root members. With `public_flat_rd/_rw`, each public variable is registered in its scope:
`VerilatedScope* s = contextp->scopeFind("TOP.wide_top.u_acc")` (5.052: `Verilated` /
`VerilatedContext::scopeFind`), then `VerilatedVar* v = s->varFind("sum")`, `v->datap()`
gives the member address in any inlining mode. The shim would fill the signal table from
`datap()` and check `v->dims()`/packed width against the IR width at create (failing with a
clear status). Not prototyped: the exact scope-name spelling and whether `public_flat` alone registers the variable (it should; `VerilatedScope` entries come from public vars) need a spike. The `.vlt` already makes the variable public, which is what creates the scope
entry. This also removes the `__DOT__`/`encodeName` coupling.

## ABI mirror (`crates/simjig/src/model/abi.rs`)
- `ModelVtable::check(*const Self, ir_hash) -> Result<&Self, AbiError>` reads `abi_version`
  and `size` through field pointers, then creates the reference. Proven with Miri: the old
  `&self` form over a 136-byte table is "constructing invalid value ... dangling reference".
- `Signal::decode() -> Result<Decoded, AbiError>` decodes storage and direction once and
  checks `storage == for_width(width)` and `words == ceil(width/32)`. Accessors take
  `Narrow::{C,S,I,Q}` (typed pointers) or `Cell::Wide`; matches are exhaustive. Miri showed
  the old catch-all doing an 8-byte read of a 1-byte member for storage 9.
- `abi_fields!` lists all 47 fields (header offsets, LP64): a const assert per field and
  `abi_fields()`, compared entry by entry with `simjig_rt_abi_fields` (w3/tracecov) in
  `every_mirror_field_matches_cpp_offsetof`.
- Miri: `cargo +nightly miri test -p simjig --lib model::abi` (both unit tests pass).

## Narrow-write test
`counter.sv` gained `tiny_in[4:0]` (C), `mid_in[11:0]` (S), `quad_in[39:0]` (Q) and
zero-extended outputs. With the mask removed, the DUT reads `[0xff, 0xffff, u64::MAX]`
instead of `[0x1f, 0xfff, 0xff_ffff_ffff]`: Verilator widens without masking. The `$fatal`
and `$stop` line numbers in the tests moved to 36 and 40.

## Merge note
`w3/tracecov` added `ShimOptions::{trace_fst, coverage}` but not the `emit.rs` caller, so
the merged tree did not build. `emit.rs` now passes `config.trace_fst` and `coverage`
(`config.coverage` or any `--coverage*` extra argument).
