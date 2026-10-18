//! simjig-build's committed generated Rust (`crates/simjig-build/tests/codegen_it/generated`),
//! compiled against the **real** `simjig::__private` instead of codegen's stub: the proof
//! that the runtime provides exactly the contract generated code names.

use simjig::__private::{Binder, ModelBinder};
use simjig_bits::{Bits, Packed};

macro_rules! generated {
    ($($name:ident)*) => {$(
        #[expect(
            dead_code,
            unreachable_pub,
            reason = "generated code is `pub` for the user crate; each test uses part of it"
        )]
        pub(crate) mod $name {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../simjig-build/tests/codegen_it/generated/",
                stringify!($name),
                ".rs"
            ));
        }
    )*};
}
generated!(arrays enums iface_top interface_modports package_params params_override producer
    scalar_vector_wide signed_ports struct_pipe structs time_precision typedef_chains wide);

#[test]
fn generated_structs_pack_through_the_runtime() {
    let inner = structs::Inner {
        tag: Bits::new(0xa),
        data: Bits::new(0x123),
    };
    assert_eq!(inner.to_raw(), 0xa123);
    assert_eq!(structs::Inner::from_raw(0xa123), inner);
    let other = structs::Inner {
        tag: Bits::new(0xb),
        ..inner
    };
    let diffs = inner.diff(&other);
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path(), "tag");
}

#[test]
fn generated_enums_report_invalid_values() {
    let invalid = structs::State::from_bits(Bits::new(3))
        .variant()
        .unwrap_err();
    assert_eq!(
        invalid.to_string(),
        "2'h3 is not a value of enum `pkg::state_t`"
    );
}

/// The generated `__bind` is generic over the runtime's binder.
#[test]
fn generated_bind_accepts_the_model_binder() {
    fn takes(_: fn(&mut ModelBinder) -> scalar_vector_wide::Dut) {}
    fn binder_is_implemented<B: Binder>() {}
    takes(scalar_vector_wide::Dut::__bind);
    binder_is_implemented::<ModelBinder>();
}
