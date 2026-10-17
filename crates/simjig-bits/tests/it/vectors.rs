//! Every operation of `Bits<N>` and `WideBits<N, W>` against the `Vec<bool>` model, at
//! widths 1, 7, 8, 9, 16, 31, 32, 33, 63, 64, 65, 127, 128, 129 and 200.

use proptest::prelude::*;
use simjig_bits::{Bit, Bits, Packed, WideBits};

use crate::reference::{self as r, Vector, make, read};

/// `from_raw` ignores bits above the width; `to_raw` returns the rest unchanged.
fn raw_round_trip<T: Vector>(raw: &[bool]) -> Result<(), TestCaseError> {
    let value = T::from_raw(<T::Raw as r::RawRef>::pack(raw));
    let n = r::width::<T>();
    prop_assert_eq!(read(value), raw[..n].to_vec());
    prop_assert_eq!(T::from_raw(value.to_raw()), value);
    Ok(())
}

fn bits_get_set<T: Vector>(a: &[bool], index: u32, value: bool) -> Result<(), TestCaseError> {
    let mut x: T = make(a);
    for (i, &b) in a.iter().enumerate() {
        prop_assert_eq!(x.bit(u32::try_from(i).unwrap()), b);
    }
    let index = index % T::WIDTH;
    x.set_bit(index, Bit::new(value));
    let mut expect = a.to_vec();
    expect[index as usize] = value;
    prop_assert_eq!(read(x), expect);
    Ok(())
}

/// `slice::<M>` and `set_slice::<M>` at a position derived from `pos`.
fn slices_of<T: Vector, const M: u32>(a: &[bool], pos: u32) -> Result<(), TestCaseError> {
    if M > T::WIDTH {
        return Ok(());
    }
    let lo = pos % (T::WIDTH - M + 1);
    let hi = lo + M - 1;
    let x: T = make(a);
    let (lo_us, hi_us) = (lo as usize, hi as usize);
    prop_assert_eq!(
        x.slice::<M>(hi, lo).to_u128(),
        r::to_u128(&a[lo_us..=hi_us])
    );

    let replacement: Vec<bool> = a.iter().rev().take(M as usize).map(|b| !b).collect();
    let mut y = x;
    y.set_slice(hi, lo, Bits::<M>::new_truncating(r::to_u128(&replacement)));
    let mut expect = a.to_vec();
    expect[lo_us..=hi_us].copy_from_slice(&replacement);
    prop_assert_eq!(read(y), expect);
    Ok(())
}

fn slices<T: Vector>(a: &[bool], pos: u32) -> Result<(), TestCaseError> {
    slices_of::<T, 1>(a, pos)?;
    slices_of::<T, 5>(a, pos)?;
    slices_of::<T, 33>(a, pos)?;
    slices_of::<T, 128>(a, pos)
}

fn arithmetic<T: Vector>(a: &[bool], b: &[bool]) -> Result<(), TestCaseError> {
    let (x, y): (T, T) = (make(a), make(b));
    prop_assert_eq!(read(x + y), r::add(a, b, false));
    prop_assert_eq!(read(x.wrapping_add(y)), r::add(a, b, false));
    prop_assert_eq!(read(x - y), r::sub(a, b));
    prop_assert_eq!(read(x.wrapping_sub(y)), r::sub(a, b));
    prop_assert_eq!(read(x & y), r::zip(a, b, |p, q| p & q));
    prop_assert_eq!(read(x | y), r::zip(a, b, |p, q| p | q));
    prop_assert_eq!(read(x ^ y), r::zip(a, b, |p, q| p ^ q));
    prop_assert_eq!(read(!x), a.iter().map(|&p| !p).collect::<Vec<_>>());
    Ok(())
}

fn comparisons<T: Vector>(a: &[bool], b: &[bool], int: u128) -> Result<(), TestCaseError> {
    let (x, y): (T, T) = (make(a), make(b));
    prop_assert_eq!(x.cmp(&y), r::cmp(a, b));
    prop_assert_eq!(x == y, a == b);
    prop_assert_eq!(x.partial_cmp(&int), Some(r::cmp_u128(a, int)));
    prop_assert_eq!(x == int, r::cmp_u128(a, int).is_eq());
    // The value's own integer, when it has one, compares equal.
    if a.len() <= 128 || a[128..].iter().all(|&p| !p) {
        prop_assert!(x == r::to_u128(a));
    }
    Ok(())
}

fn formatting<T: Vector>(a: &[bool]) -> Result<(), TestCaseError> {
    let x: T = make(a);
    let n = T::WIDTH;
    let hex = r::digits_trimmed(a, 4);
    prop_assert_eq!(x.to_string(), format!("{n}'h{}", r::digits(a, 4)));
    prop_assert_eq!(format!("{x:?}"), x.to_string());
    prop_assert_eq!(format!("{x:x}"), hex.clone());
    prop_assert_eq!(format!("{x:X}"), hex.to_uppercase());
    prop_assert_eq!(format!("{x:#x}"), format!("0x{hex}"));
    prop_assert_eq!(format!("{x:b}"), r::digits_trimmed(a, 1));
    prop_assert_eq!(
        format!("{x:#060b}").len(),
        60.max(2 + r::digits_trimmed(a, 1).len())
    );
    Ok(())
}

/// `From<u128>` accepts exactly the integers that fit and keeps their bits.
fn from_integer<T: Vector>(c: u128) -> Result<(), TestCaseError> {
    let n = r::width::<T>();
    let fits = n >= 128 || c >> n == 0;
    let converted = std::panic::catch_unwind(|| T::from(c));
    prop_assert_eq!(converted.is_ok(), fits);
    if let Ok(value) = converted {
        let mut expect: Vec<bool> = (0..128).map(|i| (c >> i) & 1 == 1).collect();
        expect.resize(n, false);
        prop_assert_eq!(read(value), expect);
    }
    Ok(())
}

/// Integers near and around `2^bits` for some `bits`, plus arbitrary ones.
fn integers() -> impl Strategy<Value = u128> {
    prop_oneof![
        any::<u128>(),
        (0u32..128, -2i8..=1)
            .prop_map(|(bits, delta)| (1u128 << bits).wrapping_add_signed(i128::from(delta))),
    ]
}

macro_rules! vector_tests {
    ($($name:ident: $ty:ty;)*) => {$(
        mod $name {
            use super::*;

            type T = $ty;

            proptest! {
                #[test]
                fn raw_round_trip(raw in r::raw_bits::<T>()) {
                    super::raw_round_trip::<T>(&raw)?;
                }

                #[test]
                fn bits_get_set(a in r::values::<T>(), index: u32, value: bool) {
                    super::bits_get_set::<T>(&a, index, value)?;
                }

                #[test]
                fn slices(a in r::values::<T>(), pos: u32) {
                    super::slices::<T>(&a, pos)?;
                }

                #[test]
                fn arithmetic(a in r::values::<T>(), b in r::values::<T>()) {
                    super::arithmetic::<T>(&a, &b)?;
                }

                #[test]
                fn comparisons(a in r::values::<T>(), b in r::values::<T>(), c in integers()) {
                    super::comparisons::<T>(&a, &b, c)?;
                }

                #[test]
                fn formatting(a in r::values::<T>()) {
                    super::formatting::<T>(&a)?;
                }

                #[test]
                fn from_integer(c in integers()) {
                    super::from_integer::<T>(c)?;
                }
            }

            #[test]
            #[should_panic(expected = "out of range")]
            fn bit_index_past_width_panics() {
                _ = T::ZERO.bit(T::WIDTH);
            }
        }
    )*};
}

vector_tests! {
    w1: Bits<1>;
    w7: Bits<7>;
    w8: Bits<8>;
    w9: Bits<9>;
    w16: Bits<16>;
    w31: Bits<31>;
    w32: Bits<32>;
    w33: Bits<33>;
    w63: Bits<63>;
    w64: Bits<64>;
    w65: Bits<65>;
    w127: Bits<127>;
    w128: Bits<128>;
    w129: WideBits<129, 5>;
    w200: WideBits<200, 7>;
}

/// The raw storage type follows Verilator's `CData`/`SData`/`IData`/`QData`/`VlWide`.
#[test]
fn raw_types_match_verilator() {
    fn raw<T: Packed>() -> &'static str {
        std::any::type_name::<T::Raw>()
    }
    assert_eq!(raw::<Bit>(), "u8");
    assert_eq!(raw::<Bits<8>>(), "u8");
    assert_eq!(raw::<Bits<9>>(), "u16");
    assert_eq!(raw::<Bits<17>>(), "u32");
    assert_eq!(raw::<Bits<33>>(), "u64");
    assert_eq!(raw::<Bits<64>>(), "u64");
    assert_eq!(raw::<Bits<65>>(), "[u32; 3]");
    assert_eq!(raw::<Bits<96>>(), "[u32; 3]");
    assert_eq!(raw::<Bits<97>>(), "[u32; 4]");
    assert_eq!(raw::<WideBits<129, 5>>(), "[u32; 5]");
}

#[test]
fn slice_checks_its_bounds() {
    let msg = |f: fn()| {
        let err = std::panic::catch_unwind(f).unwrap_err();
        err.downcast_ref::<String>().cloned().unwrap()
    };
    assert_eq!(
        msg(|| _ = Bits::<8>::MAX.slice::<2>(8, 7)),
        "slice [8:7] is out of range for width 8"
    );
    assert_eq!(
        msg(|| _ = Bits::<8>::MAX.slice::<2>(3, 4)),
        "slice [3:4] is out of range for width 8"
    );
    assert_eq!(
        msg(|| _ = WideBits::<200, 7>::MAX.slice::<4>(7, 5)),
        "slice [7:5] is 3 bits wide but the result type has 4 bits"
    );
}
