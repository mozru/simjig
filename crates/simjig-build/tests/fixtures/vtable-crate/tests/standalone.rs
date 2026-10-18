//! An integration test that never names the library: it `include!`s a generated model
//! itself, so the model archives must reach this binary without the library's rlib
//! (`cargo::rustc-link-lib` goes only to the library when there is one).
#![allow(dead_code, reason = "the generated APIs are used only in part")]

// Generated code names `::simjig::__private`; here that is the codegen tests' stub.
extern crate self as simjig;

#[path = "../src/stub.rs"]
mod __private;

/// The first field of `simjig_model_vtable`, enough to read through the pointer.
#[repr(C)]
pub struct Vtable {
    abi_version: u32,
}

mod counter {
    include!(concat!(env!("OUT_DIR"), "/simjig/counter.rs"));
}

#[test]
fn standalone_test_links_the_model() {
    let vt = counter::ffi::vtable();
    assert!(!vt.is_null());
    // SAFETY: the model's vtable is a static that lives for the whole program.
    assert_eq!(unsafe { (*vt).abi_version }, 1);
}
