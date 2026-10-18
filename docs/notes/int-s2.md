# int-s2: merging stage/1 into stage/2

## Textual conflicts
- `model/abi.rs`: stage/1's mirror wins wholesale (raw-pointer `check`, `Decoded`/`Narrow`/
  `Cell`, `abi_fields!` offset asserts, 192-byte vtable, `Status::IoError`); stage/2 only
  added `ModelVtable::prefix`, kept after the decoders.
- `tests/abi_counter.rs` and `abi_counter/shim.cpp`: shim.cpp stays deleted. The 5/12/40-bit
  ports stage/1 added to `counter.sv` reach the generated shim on their own; the pipeline
  lists them in declaration order (inputs before outputs), so the expected table was
  reordered. The IR hashes are build.rs's, not the old hand-picked constants. The
  narrow-write masking test still fails with the mask removed from `write_narrow` (checked:
  "DUT saw dirty upper bits", `[255, 65535, 2^64-1]`).
- `generated/*.rs` and `*__rust.snap`: regenerated (`SIMJIG_BLESS=1 INSTA_UPDATE=always`).
  Against stage/2 only `IR_HASH` changes; the hashes equal stage/1's.

## Semantic conflicts (merged cleanly, broke at build or run time)
- **`&'static Vtable` defeated stage/1's raw-pointer check.** stage/2's `link_model!`
  declared the symbol as returning `&'static Vtable` (a wrapper over `ModelVtable`), and
  load read `prefix` through it before checking the size: exactly the reference over a
  possibly shorter table that stage/1 made `check` avoid. `Vtable` is now
  `#[repr(transparent)] struct Vtable(*const ModelVtable)` (private field, so only a symbol
  can produce one); `ModelMeta::vtable` is `fn() -> Vtable`; `raw::check_vtable` is the one
  place it becomes a reference (unsafe stays in `abi`/`raw`, per ARCHITECTURE). Load now
  checks version, size and IR hash before the prefix, so a vtable of another model reports
  an IR-hash mismatch rather than "belongs to model" unless the hashes agree. Test:
  `a_shorter_vtable_is_rejected_before_its_prefix_is_read`.
- stage/2's fake model filled the 136-byte table; it now fills the seven new slots.
- `Status::IoError` needed an arm in `VerilatorModel::outcome` (an error, not poison).
- `LoadError::from_abi` must cover `AbiError::{Row, RowShape}` (mapped to `SignalTable`);
  `raw::validate` now decodes rows and shares the message (`meta::row_problem`).
- stage/1's new tests used the old `emit_rust(&lowered)` and `pub const K: i32`; updated for
  `RustOptions` and the absolute `::core::primitive::i32`
  (`codegen_it::verilator::iface_dut_parameters_become_dut_consts`,
  `it::pipeline::defines_reach_both_passes_alike`).

## Not verified
- Miri was not run on the abi unit tests stage/1 marks "Run under Miri".
