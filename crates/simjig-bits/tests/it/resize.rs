//! `zext`, `sext` and `trunc` against the `Vec<bool>` model, across and within the
//! `Bits`/`WideBits` boundary.

use proptest::prelude::*;
use simjig_bits::{Bits, WideBits};

use crate::reference::{self as r, make, read};

/// The model of zero- and sign-extension to `to` bits.
fn extend(a: &[bool], to: usize, sign: bool) -> Vec<bool> {
    let fill = sign && a[a.len() - 1];
    let mut out = a.to_vec();
    out.resize(to, fill);
    out
}

macro_rules! widen {
    ($($name:ident: $src:ty => $dst:ty, $zext:ident, $sext:ident;)*) => {$(
        proptest! {
            #[test]
            fn $name(a in r::values::<$src>()) {
                let x: $src = make(&a);
                let to = r::width::<$dst>();
                let z: $dst = x.$zext();
                let s: $dst = x.$sext();
                prop_assert_eq!(read(z), extend(&a, to, false));
                prop_assert_eq!(read(s), extend(&a, to, true));
            }
        }
    )*};
}

widen! {
    zext_1_to_8: Bits<1> => Bits<8>, zext, sext;
    zext_7_to_8: Bits<7> => Bits<8>, zext, sext;
    zext_8_to_9: Bits<8> => Bits<9>, zext, sext;
    zext_8_to_8: Bits<8> => Bits<8>, zext, sext;
    zext_16_to_31: Bits<16> => Bits<31>, zext, sext;
    zext_31_to_33: Bits<31> => Bits<33>, zext, sext;
    zext_32_to_64: Bits<32> => Bits<64>, zext, sext;
    zext_63_to_65: Bits<63> => Bits<65>, zext, sext;
    zext_64_to_128: Bits<64> => Bits<128>, zext, sext;
    zext_127_to_128: Bits<127> => Bits<128>, zext, sext;
    zext_1_to_129: Bits<1> => WideBits<129, 5>, zext_wide, sext_wide;
    zext_65_to_200: Bits<65> => WideBits<200, 7>, zext_wide, sext_wide;
    zext_128_to_129: Bits<128> => WideBits<129, 5>, zext_wide, sext_wide;
    zext_129_to_200: WideBits<129, 5> => WideBits<200, 7>, zext, sext;
    zext_200_to_200: WideBits<200, 7> => WideBits<200, 7>, zext, sext;
}

macro_rules! narrow {
    ($($name:ident: $src:ty => $dst:ty, $trunc:ident;)*) => {$(
        proptest! {
            #[test]
            fn $name(a in r::values::<$src>()) {
                let x: $src = make(&a);
                let t: $dst = x.$trunc();
                prop_assert_eq!(read(t), a[..r::width::<$dst>()].to_vec());
            }
        }
    )*};
}

narrow! {
    trunc_8_to_1: Bits<8> => Bits<1>, trunc;
    trunc_9_to_8: Bits<9> => Bits<8>, trunc;
    trunc_33_to_32: Bits<33> => Bits<32>, trunc;
    trunc_65_to_64: Bits<65> => Bits<64>, trunc;
    trunc_128_to_127: Bits<128> => Bits<127>, trunc;
    trunc_128_to_128: Bits<128> => Bits<128>, trunc;
    trunc_200_to_129: WideBits<200, 7> => WideBits<129, 5>, trunc;
    trunc_129_to_128: WideBits<129, 5> => Bits<128>, trunc_bits;
    trunc_200_to_65: WideBits<200, 7> => Bits<65>, trunc_bits;
    trunc_200_to_1: WideBits<200, 7> => Bits<1>, trunc_bits;
}
