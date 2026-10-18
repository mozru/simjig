//! Model FFI: the C ABI to generated Verilator models.
//!
//! `abi` mirrors `simjig_abi.h`. Nothing in the runtime uses it yet (the kernel and the typed
//! `Dut` come later), so it is compiled but dead in the library.

#[expect(
    dead_code,
    reason = "the ABI mirror lands before the runtime that uses it"
)]
pub(crate) mod abi;
