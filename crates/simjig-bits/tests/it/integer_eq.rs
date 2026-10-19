//! `impl PartialEq<Bits<N>> for u32` (and the other integers) makes `u32: PartialEq<?T>`
//! ambiguous, so comparisons whose right side is inferred from the left stop compiling once
//! simjig-bits is linked.

#[test]
fn an_inferred_right_hand_side_still_compiles() {
    assert_eq!(simjig_bits::Bits::<4>::ZERO, 0); // link the impls
    let n: u32 = 3;
    assert_eq!(n, "3".parse().unwrap());
    let total: u64 = 6;
    assert_eq!(total, [1u64, 2, 3].iter().sum());
    let byte = 3_u8;
    let widened = n == byte.into();
    assert!(widened);
}
