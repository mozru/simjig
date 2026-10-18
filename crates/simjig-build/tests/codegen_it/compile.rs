//! The committed generated Rust (`generated/*.rs`), compiled against the stub runtime,
//! and its behaviour: pack/unpack against the IR offsets, enums, unions, `diff`, constants
//! and `Dut` binding.

use proptest::prelude::*;
use simjig_bits::{Bit, Bits, Packed, WideBits};
use simjig_build::codegen::{ItemKind, Lowered, StructField, lower};

use crate::__private::{Access, RawWords, RecordingBinder};
use crate::cases;

macro_rules! generated {
    ($($name:ident)*) => {$(
        #[expect(dead_code, reason = "each test uses only part of the generated API")]
        pub mod $name {
            include!(concat!("generated/", stringify!($name), ".rs"));
        }
    )*};
}
generated!(arrays enums iface_top interface_modports package_params params_override producer
    scalar_vector_wide signed_ports struct_pipe structs time_precision typedef_chains wide);

/// The fields of the struct named `sv_name` (as lowered from the IR).
fn ir_fields(lowered: &Lowered, sv_name: &str) -> Vec<StructField> {
    let item = lowered.items.iter().find(|i| i.sv_name == sv_name).unwrap();
    let ItemKind::Struct { fields } = &item.kind else {
        panic!("{sv_name} is not a struct")
    };
    fields.clone()
}

/// The raw storage of `value` as words.
fn words_of<T: Packed>(value: T) -> Vec<u32>
where
    T::Raw: RawWords,
{
    let mut out = vec![0u32; T::WIDTH.div_ceil(32) as usize];
    value.to_raw().to_words(&mut out);
    out
}

/// Bits `lsb .. lsb + width` of `words`, as words: an independent bit-by-bit reference.
fn slice_bits(words: &[u32], lsb: u32, width: u32) -> Vec<u32> {
    let mut out = vec![0u32; width.div_ceil(32) as usize];
    for i in 0..width {
        let b = lsb + i;
        if words[(b / 32) as usize] >> (b % 32) & 1 == 1 {
            out[(i / 32) as usize] |= 1 << (i % 32);
        }
    }
    out
}

/// `words` with every bit at `width` and above cleared.
fn clean(words: &[u32], width: u32) -> Vec<u32> {
    slice_bits(words, 0, width)
}

/// Checks one struct type against its IR: from random raw storage (with dirty upper bits),
/// every field holds exactly the bits at its IR offset, and `to_raw` gives back the
/// storage with the upper bits cleared.
macro_rules! struct_matches_ir {
    ($test:ident, $case:expr, $sv:literal, $ty:ty, [$($field:ident),*]) => {
        proptest! {
            #[test]
            fn $test(words in prop::collection::vec(any::<u32>(), 10)) {
                // Elaborating once per test, not once per proptest case.
                static IR: std::sync::OnceLock<Vec<StructField>> = std::sync::OnceLock::new();
                let ir = IR.get_or_init(|| {
                    let case = $case;
                    ir_fields(&lower(&case.design, &case.config).unwrap(), $sv)
                });
                let width = <$ty as Packed>::WIDTH;
                let n = width.div_ceil(32) as usize;
                let value = <$ty as Packed>::from_raw(RawWords::from_words(&words[..n]));
                let got: Vec<(&str, Vec<u32>)> = vec![$((stringify!($field), words_of(value.$field))),*];
                prop_assert_eq!(got.len(), ir.len());
                for ((name, field_words), f) in got.iter().zip(ir) {
                    prop_assert_eq!(*name, f.name.as_str());
                    prop_assert_eq!(field_words, &slice_bits(&words, f.lsb, f.width), "field {}", name);
                }
                let mut written = vec![0u32; n];
                value.to_raw().to_words(&mut written);
                prop_assert_eq!(written, clean(&words[..n], width));
            }
        }
    };
}

struct_matches_ir!(
    inner_matches_ir,
    cases::fixture("structs"),
    "pkg::inner_t",
    structs::Inner,
    [tag, data]
);
struct_matches_ir!(
    outer_matches_ir,
    cases::fixture("structs"),
    "pkg::outer_t",
    structs::Outer,
    [r#in, st, valid, pad]
);
struct_matches_ir!(
    signed_struct_matches_ir,
    cases::fixture("structs"),
    "pkg::sdata_t",
    structs::Sdata,
    [hi, lo]
);
struct_matches_ir!(
    anon_struct_matches_ir,
    cases::fixture("structs"),
    "top.in_anon",
    structs::InAnon,
    [a, b]
);
struct_matches_ir!(
    beat_matches_ir,
    cases::struct_pipe(),
    "struct_pipe_pkg::beat_t",
    struct_pipe::Beat,
    [op, tag, payload]
);
struct_matches_ir!(
    part_matches_ir,
    cases::wide(),
    "wide_pkg::part_t",
    wide::PartT,
    [lo33, mode, mid70, self_]
);
struct_matches_ir!(
    big_matches_ir,
    cases::wide(),
    "wide_pkg::big_t",
    wide::Big,
    [a, r#loop, b]
);
struct_matches_ir!(
    nib_matches_ir,
    cases::wide(),
    "wide_pkg::nib_t",
    wide::Nib,
    [hi, lo]
);

proptest! {
    #[test]
    fn struct_round_trips(lo33 in any::<u64>(), mode in 0u8..8, mid70 in any::<u128>(), flag: bool) {
        let part = wide::PartT {
            lo33: Bits::new_truncating(u128::from(lo33)),
            mode: wide::Mode::from_bits(Bits::new_truncating(u128::from(mode))),
            mid70: Bits::new_truncating(mid70),
            self_: Bit::new(flag),
        };
        let big = wide::Big { a: part, r#loop: Bits::new_truncating(mid70.rotate_left(7)), b: part };
        prop_assert_eq!(wide::PartT::from_raw(part.to_raw()), part);
        prop_assert_eq!(wide::Big::from_raw(big.to_raw()), big);
        // The union's struct view is the same bits as the struct.
        prop_assert_eq!(wide::PartU::from_part(part).part(), part);
        prop_assert_eq!(wide::PartU::from_part(part).raw_2().to_raw(), part.to_raw());
    }
}

#[test]
fn enums_keep_unknown_values() {
    use struct_pipe::{Op, OpVariant};
    assert_eq!(Op::ADD.variant(), Ok(OpVariant::Add));
    assert_eq!(Op::from(OpVariant::Inv), Op::INV);
    assert_eq!(format!("{:?}", Op::XOR), "OP_XOR");
    // 3'h6 names no variant of mode_e: reading it neither fails nor loses the bits.
    let odd = wide::Mode::from_raw(6);
    let err = odd.variant().unwrap_err();
    assert_eq!(
        (err.type_name, err.width, err.value),
        ("wide_pkg::mode_e", 3, 6)
    );
    assert_eq!(format!("{odd:?}"), "Mode(3'h6)");
    assert_eq!(odd.to_raw(), 6);
    // One-bit enum over `Bit`.
    assert_eq!(enums::IAnon::YES.variant(), Ok(enums::IAnonVariant::Yes));
    assert_eq!(enums::IAnon::from_raw(1), enums::IAnon::YES);
}

#[test]
fn union_views_share_bits() {
    let inner = structs::Inner {
        tag: Bits::new(0xa),
        data: Bits::new(0x123),
    };
    let word = structs::Word::from_as_inner(inner);
    assert_eq!(word.word(), Bits::<16>::new(0xa123));
    assert_eq!(word.bytes(), Bits::<16>::new(0xa123));
    assert_eq!(word.as_inner(), inner);
    assert_eq!(
        format!("{word:?}"),
        "Word { word: 16'ha123, bytes: 16'ha123, as_inner: Inner { tag: 4'ha, data: 12'h123 } }"
    );
}

#[test]
fn diff_names_nested_fields() {
    let a = structs::Outer::default();
    let mut b = a;
    b.r#in.tag = Bits::new(3);
    b.st = structs::State::DONE;
    let diffs: Vec<(String, String, String)> = a
        .diff(&b)
        .into_iter()
        .map(|d| (d.path, d.left, d.right))
        .collect();
    assert_eq!(
        diffs,
        [
            ("in.tag".into(), "4'h0".into(), "4'h3".into()),
            ("st".into(), "IDLE".into(), "DONE".into()),
        ]
    );
    assert_eq!(a.diff(&a), []);
}

#[test]
fn constants_carry_ir_values() {
    use package_params::cfg;
    use params_override::Dut;
    assert_eq!(
        (cfg::WIDTH, cfg::NEG, cfg::MAGIC, cfg::FLAG, cfg::DEPTH),
        (12, -5, 0xa5, true, 24)
    );
    assert_eq!(cfg::BIG, (1u128 << 99) | 1);
    assert_eq!((cfg::NAME, cfg::RATIO), ("simjig", 0.25));
    let word: package_params::Word = Bits::new(cfg::WIDTH.unsigned_abs().into());
    assert_eq!(word, Bits::<12>::new(12));
    assert_eq!((Dut::W, Dut::MODE, Dut::LABEL), (16, 0xc, "over"));
    assert_eq!(Dut::WIDE, 1);
    let raw = wide::Big::default().to_raw();
    assert_eq!(WideBits::<307, 10>::from_words(raw), WideBits::ZERO);
}

#[test]
fn bind_hands_each_field_its_slot() {
    for (case, (calls, metas)) in [
        (cases::wide(), bind_wide()),
        (cases::iface_top(), bind_iface_top()),
    ] {
        let lowered = lower(&case.design, &case.config).unwrap();
        let expected: Vec<(u32, Access)> = lowered
            .signals
            .iter()
            .map(|s| (s.slot, stub_access(s.access)))
            .collect();
        assert_eq!(calls, expected, "{}", lowered.top);
        let names: Vec<&str> = lowered.signals.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(metas, names, "{}", lowered.top);
    }
}

#[test]
fn inout_ports_bind_as_inout() {
    let mut binder = RecordingBinder::default();
    let dut = scalar_vector_wide::Dut::__bind(&mut binder);
    assert_eq!(
        (dut.bidir.meta.name, dut.bidir.meta.access),
        ("top.bidir", Access::InOut)
    );
    // `\esc$name` and the C++ keyword `char` get legal Rust names.
    assert_eq!(
        (dut.esc_name.meta.name, dut.char.meta.name),
        ("top.esc$name", "top.char")
    );
}

#[test]
fn ir_hash_matches_the_lowering() {
    let hashes = [
        (cases::fixture("structs"), structs::Dut::IR_HASH),
        (cases::wide(), wide::Dut::IR_HASH),
        (cases::iface_top(), iface_top::Dut::IR_HASH),
    ];
    for (case, hash) in hashes {
        assert_eq!(
            lower(&case.design, &case.config).unwrap().ir_hash,
            hash,
            "{}",
            case.name
        );
    }
}

fn stub_access(access: simjig_build::codegen::Access) -> Access {
    use simjig_build::codegen::Access as A;
    match access {
        A::In => Access::In,
        A::Out => Access::Out,
        A::InOut => Access::InOut,
        A::PublicRd => Access::PublicRd,
        A::PublicRw => Access::PublicRw,
    }
}

fn bind_wide() -> (Vec<(u32, Access)>, Vec<&'static str>) {
    let mut binder = RecordingBinder::default();
    let dut = wide::Dut::__bind(&mut binder);
    assert_eq!(
        (
            dut.r#match.meta.slot,
            dut.knob.meta.slot,
            dut.u_acc_sum.meta.slot
        ),
        (3, 9, 10)
    );
    assert_eq!(dut.state.meta.access, Access::PublicRd);
    let names = wide::Dut::SIGNALS.iter().map(|s| s.name).collect();
    (binder.calls, names)
}

fn bind_iface_top() -> (Vec<(u32, Access)>, Vec<&'static str>) {
    let mut binder = RecordingBinder::default();
    let dut = iface_top::Dut::__bind(&mut binder);
    assert_eq!(
        dut.in_bus.ready.meta.name,
        "simjig_wrap_iface_top.in_bus_ready"
    );
    assert_eq!(dut.out_bus.ready.meta.access, Access::In);
    let names = iface_top::Dut::SIGNALS.iter().map(|s| s.name).collect();
    (binder.calls, names)
}
