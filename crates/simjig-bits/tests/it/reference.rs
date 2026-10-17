//! A `Vec<bool>` reference model (index 0 is the least significant bit) and the glue that
//! lets one generic property test run against `Bits<N>` and `WideBits<N, W>`.
//!
//! The model never calls the code under test: raw storage is packed and unpacked here by
//! hand, so a property comparing the two is an independent check.

use std::cmp::Ordering;
use std::fmt::{Binary, Debug, Display, LowerHex, UpperHex};
use std::ops::{Add, BitAnd, BitOr, BitXor, Not, Sub};

use proptest::collection::vec;
use proptest::prelude::*;
use simjig_bits::{Bit, Bits, Packed, WideBits};

/// Verilator storage, packed and unpacked independently of the crate.
pub(crate) trait RawRef: Copy + Eq + Debug {
    /// Bits in the storage type.
    const BITS: usize;
    /// Packs `bits` (at most `BITS` of them; missing ones are zero).
    fn pack(bits: &[bool]) -> Self;
    /// All `BITS` bits.
    fn unpack(self) -> Vec<bool>;
}

macro_rules! raw_int {
    ($($t:ty)*) => {$(
        impl RawRef for $t {
            const BITS: usize = <$t>::BITS as usize;
            fn pack(bits: &[bool]) -> Self {
                bits.iter().rev().fold(0, |acc, &b| (acc << 1) | <$t>::from(b))
            }
            fn unpack(self) -> Vec<bool> {
                (0..Self::BITS).map(|i| (self >> i) & 1 == 1).collect()
            }
        }
    )*};
}
raw_int!(u8 u16 u32 u64);

impl<const W: usize> RawRef for [u32; W] {
    const BITS: usize = 32 * W;
    fn pack(bits: &[bool]) -> Self {
        std::array::from_fn(|w| {
            let chunk = bits.get(32 * w..).unwrap_or(&[]);
            u32::pack(&chunk[..chunk.len().min(32)])
        })
    }
    fn unpack(self) -> Vec<bool> {
        self.iter().flat_map(|w| w.unpack()).collect()
    }
}

/// The operations under test, implemented by every vector type.
pub(crate) trait Vector:
    Packed<Raw: RawRef>
    + Debug
    + Display
    + LowerHex
    + UpperHex
    + Binary
    + Ord
    + PartialEq<u128>
    + PartialOrd<u128>
    + From<u128>
    + Add<Output = Self>
    + Sub<Output = Self>
    + BitAnd<Output = Self>
    + BitOr<Output = Self>
    + BitXor<Output = Self>
    + Not<Output = Self>
{
    fn bit(self, index: u32) -> Bit;
    fn set_bit(&mut self, index: u32, value: Bit);
    fn slice<const M: u32>(self, hi: u32, lo: u32) -> Bits<M>;
    fn set_slice<const M: u32>(&mut self, hi: u32, lo: u32, value: Bits<M>);
    fn wrapping_add(self, rhs: Self) -> Self;
    fn wrapping_sub(self, rhs: Self) -> Self;
}

macro_rules! forward_vector {
    () => {
        fn bit(self, index: u32) -> Bit {
            self.bit(index)
        }
        fn set_bit(&mut self, index: u32, value: Bit) {
            self.set_bit(index, value);
        }
        fn slice<const M: u32>(self, hi: u32, lo: u32) -> Bits<M> {
            self.slice(hi, lo)
        }
        fn set_slice<const M: u32>(&mut self, hi: u32, lo: u32, value: Bits<M>) {
            self.set_slice(hi, lo, value);
        }
        fn wrapping_add(self, rhs: Self) -> Self {
            self.wrapping_add(rhs)
        }
        fn wrapping_sub(self, rhs: Self) -> Self {
            self.wrapping_sub(rhs)
        }
    };
}

impl<const N: u32> Vector for Bits<N>
where
    Bits<N>: Packed<Raw: RawRef>,
{
    forward_vector!();
}

impl<const N: u32, const W: usize> Vector for WideBits<N, W> {
    forward_vector!();
}

/// The width of `T` as a length.
pub(crate) fn width<T: Packed>() -> usize {
    T::WIDTH as usize
}

/// Random values of `T`'s width.
pub(crate) fn values<T: Packed>() -> impl Strategy<Value = Vec<bool>> {
    vec(any::<bool>(), width::<T>())
}

/// Random raw storage for `T`, with random bits above the width too.
pub(crate) fn raw_bits<T: Packed<Raw: RawRef>>() -> impl Strategy<Value = Vec<bool>> {
    vec(any::<bool>(), <T::Raw as RawRef>::BITS)
}

/// Builds a value from reference bits through `from_raw`.
pub(crate) fn make<T: Packed<Raw: RawRef>>(bits: &[bool]) -> T {
    assert_eq!(bits.len(), width::<T>());
    T::from_raw(T::Raw::pack(bits))
}

/// Reads a value back through `to_raw`, checking that the bits above the width are zero.
pub(crate) fn read<T: Packed<Raw: RawRef>>(value: T) -> Vec<bool> {
    let mut bits = value.to_raw().unpack();
    let upper = bits.split_off(width::<T>());
    assert!(upper.iter().all(|&b| !b), "to_raw left upper bits set");
    bits
}

/// `a + b + carry_in`, dropping the final carry.
pub(crate) fn add(a: &[bool], b: &[bool], carry_in: bool) -> Vec<bool> {
    let mut carry = carry_in;
    a.iter()
        .zip(b)
        .map(|(&x, &y)| {
            let sum = x ^ y ^ carry;
            carry = (x && y) || (carry && (x ^ y));
            sum
        })
        .collect()
}

/// `a - b`, as `a + !b + 1` (two's complement).
pub(crate) fn sub(a: &[bool], b: &[bool]) -> Vec<bool> {
    let not_b: Vec<bool> = b.iter().map(|&x| !x).collect();
    add(a, &not_b, true)
}

/// Element-wise `f`.
pub(crate) fn zip(a: &[bool], b: &[bool], f: impl Fn(bool, bool) -> bool) -> Vec<bool> {
    a.iter().zip(b).map(|(&x, &y)| f(x, y)).collect()
}

/// Unsigned comparison of two equal-length vectors.
pub(crate) fn cmp(a: &[bool], b: &[bool]) -> Ordering {
    a.iter().rev().cmp(b.iter().rev())
}

/// Unsigned comparison with an integer.
pub(crate) fn cmp_u128(a: &[bool], other: u128) -> Ordering {
    let other: Vec<bool> = (0..a.len().max(128))
        .map(|i| i < 128 && (other >> i) & 1 == 1)
        .collect();
    let mut a = a.to_vec();
    a.resize(other.len(), false);
    cmp(&a, &other)
}

/// The low 128 bits as an integer.
pub(crate) fn to_u128(bits: &[bool]) -> u128 {
    bits.iter()
        .take(128)
        .rev()
        .fold(0, |acc, &b| (acc << 1) | u128::from(b))
}

/// Digits in base `2^bits_per_digit`, most significant first, exactly enough for every
/// bit.
pub(crate) fn digits(bits: &[bool], bits_per_digit: usize) -> String {
    bits.chunks(bits_per_digit)
        .rev()
        .map(|chunk| {
            let digit = chunk
                .iter()
                .rev()
                .fold(0u32, |acc, &b| (acc << 1) | u32::from(b));
            char::from_digit(digit, 16).unwrap()
        })
        .collect()
}

/// `digits` without leading zeros (but at least one digit), as Rust integers format.
pub(crate) fn digits_trimmed(bits: &[bool], bits_per_digit: usize) -> String {
    let all = digits(bits, bits_per_digit);
    let trimmed = all.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}
