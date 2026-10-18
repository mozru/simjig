//! Model FFI: the C ABI to generated Verilator models.
//!
//! `abi` mirrors `simjig_abi.h`. Nothing in the runtime uses it yet (the kernel and the typed
//! `Dut` come later), so it is compiled but dead in the library.

#[expect(
    dead_code,
    reason = "the ABI mirror lands before the runtime that uses it"
)]
#[expect(
    unsafe_code,
    reason = "FFI mirror of simjig_abi.h: raw member pointers and extern vtables"
)]
pub(crate) mod abi;
