//! Compile test: linking simjig-bits must not break inference in ordinary Rust. Each line
//! below fails with E0283 if simjig-bits implements `PartialEq<Bits<N>>` (or `WideBits`,
//! `Bit`) for that line's left-hand type, so this file not compiling is the failure.

use simjig_bits::{Bit, Bits};

#[test]
fn std_comparisons_with_an_inferred_right_side_still_compile() {
    assert!(Bits::<4>::ZERO == 0 && Bit::ZERO == 0); // link the impls
    let xs = [1_u32, 2, 3];
    let total: u32 = 6;
    assert_eq!(total, xs.iter().sum());
    let n: u64 = 3;
    assert_eq!(n, xs.len().try_into().unwrap());
    // `==` itself, not `assert_eq!`, which infers through `&T: Debug` too.
    let k: usize = 3;
    let parsed = k == "3".parse().unwrap();
    let byte: u8 = 0;
    let defaulted = byte == Default::default();
    let f: i32 = 2;
    let converted = f == From::from(2_i8);
    let flag = false;
    let unset = flag == Default::default();
    assert!(parsed && defaulted && converted && unset);
}

/// The one integer on the left that does compare with a value: an unsuffixed literal.
#[test]
fn a_literal_on_the_left_still_infers() {
    let x = Bits::<8>::new(4);
    assert_eq!(4, x);
    let from_the_left = 4 == x;
    assert!(from_the_left);
}
