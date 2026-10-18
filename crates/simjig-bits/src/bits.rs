//! [`Bits<N>`]: an unsigned 2-state bit vector of 1 to 128 bits, stored in a `u128`.
//!
//! Invariant: every storage bit at position `N` or above is zero. Every constructor masks
//! or checks, and every operation preserves it, so equality, hashing and ordering can use
//! the storage directly.
//!
//! Width is part of the type and never changes implicitly: arithmetic and bitwise
//! operators take and return `Bits<N>` of one `N`, and width changes are the explicit
//! [`zext`](Bits::zext), [`sext`](Bits::sext) and [`trunc`](Bits::trunc).

use core::fmt;
use core::ops::{
    Add, AddAssign, BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not, Sub,
    SubAssign,
};

use crate::bit::Bit;
use crate::error::OutOfRange;
use crate::packed::Packed;
use crate::wide::WideBits;
use crate::words;

/// An unsigned 2-state bit vector of `N` bits, `1 <= N <= 128`.
///
/// A width outside `1..=128` fails to compile as soon as a value is constructed (the
/// error is reported by `cargo build`, not by `cargo check`). Use [`WideBits`] above 128
/// bits.
///
/// # Examples
///
/// ```
/// use simjig_bits::Bits;
///
/// let a = Bits::<8>::new(0xf0);
/// let b: Bits<8> = 0x21.into();
/// assert_eq!(a + b, 0x11); // wraps at 8 bits
/// assert_eq!((a | b).to_string(), "8'hf1");
/// assert_eq!(a.slice::<4>(7, 4), 0xf);
/// ```
///
/// ```compile_fail,E0080
/// let _ = simjig_bits::Bits::<0>::ZERO;
/// ```
///
/// ```compile_fail,E0080
/// let _ = simjig_bits::Bits::<129>::new(0);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[must_use]
pub struct Bits<const N: u32>(u128);

impl<const N: u32> Bits<N> {
    /// The width in bits.
    pub const WIDTH: u32 = N;

    /// Ones in the low `N` bits. Every constructor uses it, so it also carries the
    /// compile-time width check.
    const MASK: u128 = {
        assert!(
            N >= 1 && N <= 128,
            "Bits<N> needs 1 <= N <= 128; use WideBits above 128"
        );
        u128::MAX >> (128 - N)
    };

    /// All bits `0`.
    pub const ZERO: Self = Self::new_truncating(0);
    /// All bits `1`, the largest value.
    pub const MAX: Self = Self::new_truncating(u128::MAX);

    /// Converts `value`, panicking if it needs more than `N` bits.
    ///
    /// # Panics
    ///
    /// Panics at the caller's location if `value >= 2^N`; the message names the width.
    #[track_caller]
    pub fn new(value: u128) -> Self {
        match Self::try_new(value) {
            Ok(bits) => bits,
            Err(err) => panic!("Bits<{N}>: {err}"),
        }
    }

    /// Converts `value`, or returns an error if it needs more than `N` bits.
    pub const fn try_new(value: u128) -> Result<Self, OutOfRange> {
        if value & !Self::MASK == 0 {
            Ok(Self(value))
        } else {
            Err(OutOfRange::too_large(value, N))
        }
    }

    /// Keeps the low `N` bits of `value` and drops the rest.
    pub const fn new_truncating(value: u128) -> Self {
        Self(value & Self::MASK)
    }

    /// The value as an unsigned integer.
    pub const fn to_u128(self) -> u128 {
        self.0
    }

    /// The value of bit `index` (bit 0 is the least significant).
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    #[track_caller]
    pub fn bit(self, index: u32) -> Bit {
        check_index(index, N);
        Bit::new((self.0 >> index) & 1 == 1)
    }

    /// Sets bit `index` to `value`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    #[track_caller]
    pub fn set_bit(&mut self, index: u32, value: impl Into<Bit>) {
        check_index(index, N);
        let bit = u128::from(value.into().to_bool());
        self.0 = (self.0 & !(1 << index)) | (bit << index);
    }

    /// The bits `hi..=lo` (SystemVerilog `x[hi:lo]`) as an `M`-bit value.
    ///
    /// # Panics
    ///
    /// Panics unless `lo <= hi < N` and `M == hi - lo + 1`.
    ///
    /// # Examples
    ///
    /// ```
    /// use simjig_bits::Bits;
    ///
    /// assert_eq!(Bits::<16>::new(0xabcd).slice::<8>(11, 4), 0xbc);
    /// ```
    #[track_caller]
    pub fn slice<const M: u32>(self, hi: u32, lo: u32) -> Bits<M> {
        check_slice(hi, lo, N, M);
        Bits::new_truncating(self.0 >> lo)
    }

    /// Replaces the bits `hi..=lo` (SystemVerilog `x[hi:lo] = value`).
    ///
    /// # Panics
    ///
    /// Panics unless `lo <= hi < N` and `M == hi - lo + 1`.
    #[track_caller]
    pub fn set_slice<const M: u32>(&mut self, hi: u32, lo: u32, value: Bits<M>) {
        check_slice(hi, lo, N, M);
        let field = Bits::<M>::MASK << lo;
        self.0 = (self.0 & !field) | (value.0 << lo);
    }

    /// Zero-extends to `M >= N` bits.
    ///
    /// `M < N` fails to compile (when the call is built).
    ///
    /// # Examples
    ///
    /// ```
    /// use simjig_bits::Bits;
    ///
    /// let y: Bits<16> = Bits::<8>::new(0x80).zext();
    /// assert_eq!(y, 0x0080);
    /// ```
    ///
    /// ```compile_fail,E0080
    /// let _ = simjig_bits::Bits::<8>::ZERO.zext::<4>();
    /// ```
    pub fn zext<const M: u32>(self) -> Bits<M> {
        const { assert!(M >= N, "zext target must not be narrower than the source") };
        Bits::new_truncating(self.0)
    }

    /// Sign-extends to `M >= N` bits, copying bit `N - 1` into the new upper bits.
    ///
    /// # Examples
    ///
    /// ```
    /// use simjig_bits::Bits;
    ///
    /// assert_eq!(Bits::<8>::new(0x80).sext::<16>(), 0xff80);
    /// assert_eq!(Bits::<8>::new(0x7f).sext::<16>(), 0x007f);
    /// ```
    pub fn sext<const M: u32>(self) -> Bits<M> {
        const { assert!(M >= N, "sext target must not be narrower than the source") };
        Bits::new_truncating(self.sign_extended())
    }

    /// Keeps the low `M <= N` bits.
    ///
    /// # Examples
    ///
    /// ```
    /// use simjig_bits::Bits;
    ///
    /// assert_eq!(Bits::<16>::new(0xabcd).trunc::<8>(), 0xcd);
    /// ```
    pub fn trunc<const M: u32>(self) -> Bits<M> {
        const { assert!(M <= N, "trunc target must not be wider than the source") };
        Bits::new_truncating(self.0)
    }

    /// Zero-extends to a [`WideBits`] of `M > 128` bits.
    pub fn zext_wide<const M: u32, const W: usize>(self) -> WideBits<M, W> {
        WideBits::from_u128_words(words::from_u128(self.0), false)
    }

    /// Sign-extends to a [`WideBits`] of `M > 128` bits.
    pub fn sext_wide<const M: u32, const W: usize>(self) -> WideBits<M, W> {
        let negative = self.0 >> (N - 1) == 1;
        WideBits::from_u128_words(words::from_u128(self.sign_extended()), negative)
    }

    /// Storage with bit `N - 1` copied into every bit above it.
    const fn sign_extended(self) -> u128 {
        if self.0 >> (N - 1) == 1 {
            self.0 | !Self::MASK
        } else {
            self.0
        }
    }

    /// `self + rhs`, wrapping at `N` bits (SystemVerilog `+` at equal widths).
    pub const fn wrapping_add(self, rhs: Self) -> Self {
        Self::new_truncating(self.0.wrapping_add(rhs.0))
    }

    /// `self - rhs`, wrapping at `N` bits (SystemVerilog `-` at equal widths).
    pub const fn wrapping_sub(self, rhs: Self) -> Self {
        Self::new_truncating(self.0.wrapping_sub(rhs.0))
    }
}

/// Panics unless `index` names a bit of a `width`-bit value.
#[track_caller]
pub(crate) fn check_index(index: u32, width: u32) {
    assert!(
        index < width,
        "bit index {index} is out of range for width {width}"
    );
}

/// Panics unless `hi..=lo` is a range of a `width`-bit value holding `result` bits.
#[track_caller]
pub(crate) fn check_slice(hi: u32, lo: u32, width: u32, result: u32) {
    assert!(
        lo <= hi && hi < width,
        "slice [{hi}:{lo}] is out of range for width {width}"
    );
    assert!(
        hi - lo + 1 == result,
        "slice [{hi}:{lo}] is {} bits wide but the result type has {result} bits",
        hi - lo + 1
    );
}

impl<const N: u32> Default for Bits<N> {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Converts an integer, typically a literal.
///
/// This is the only integer type that converts into `Bits<N>` with `From`, so an untyped
/// literal (`sig.set(0x5a)`) infers `u128` and any width up to 128 accepts any literal
/// that fits. Negative literals fail to compile. Other integer types use `TryFrom`.
///
/// # Panics
///
/// Panics at the caller's location if `value >= 2^N`; the message names the width.
impl<const N: u32> From<u128> for Bits<N> {
    #[track_caller]
    fn from(value: u128) -> Self {
        Self::new(value)
    }
}

impl From<Bit> for Bits<1> {
    fn from(value: Bit) -> Self {
        Self(u128::from(value.to_bool()))
    }
}

impl From<Bits<1>> for Bit {
    fn from(value: Bits<1>) -> Self {
        Bit::new(value.0 == 1)
    }
}

macro_rules! bits_try_from_unsigned {
    ($($int:ty)*) => {$(
        impl<const N: u32> TryFrom<$int> for Bits<N> {
            type Error = OutOfRange;
            fn try_from(value: $int) -> Result<Self, OutOfRange> {
                Self::try_new(u128::from(value))
            }
        }
    )*};
}
bits_try_from_unsigned!(u8 u16 u32 u64);

macro_rules! bits_try_from_signed {
    ($($int:ty)*) => {$(
        impl<const N: u32> TryFrom<$int> for Bits<N> {
            type Error = OutOfRange;
            fn try_from(value: $int) -> Result<Self, OutOfRange> {
                match u128::try_from(value) {
                    Ok(value) => Self::try_new(value),
                    Err(_) => Err(OutOfRange::negative(i128::from(value), N)),
                }
            }
        }
    )*};
}
bits_try_from_signed!(i8 i16 i32 i64 i128);

impl<const N: u32> PartialEq<u128> for Bits<N> {
    fn eq(&self, other: &u128) -> bool {
        self.0 == *other
    }
}

/// `4 == x` as well as `x == 4`, so `assert_eq!(4, x.get())` compiles: an integer equals a
/// value when it is the same number (a negative integer equals none).
macro_rules! int_eq_bits {
    ($($int:ty)*) => {$(
        impl<const N: u32> PartialEq<Bits<N>> for $int {
            fn eq(&self, other: &Bits<N>) -> bool {
                u128::try_from(*self).is_ok_and(|v| *other == v)
            }
        }
    )*};
}
int_eq_bits!(u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize);

impl<const N: u32> PartialOrd<u128> for Bits<N> {
    fn partial_cmp(&self, other: &u128) -> Option<core::cmp::Ordering> {
        Some(self.0.cmp(other))
    }
}

impl<const N: u32> Not for Bits<N> {
    type Output = Self;
    fn not(self) -> Self {
        Self::new_truncating(!self.0)
    }
}

macro_rules! bits_binop {
    ($($trait:ident $method:ident $assign:ident $assign_method:ident |$a:ident, $b:ident| $body:expr;)*) => {$(
        impl<const N: u32> $trait for Bits<N> {
            type Output = Self;
            fn $method(self, rhs: Self) -> Self {
                let ($a, $b) = (self, rhs);
                $body
            }
        }
        impl<const N: u32> $assign for Bits<N> {
            fn $assign_method(&mut self, rhs: Self) {
                *self = $trait::$method(*self, rhs);
            }
        }
    )*};
}
bits_binop! {
    Add add AddAssign add_assign |a, b| a.wrapping_add(b);
    Sub sub SubAssign sub_assign |a, b| a.wrapping_sub(b);
    BitAnd bitand BitAndAssign bitand_assign |a, b| Self(a.0 & b.0);
    BitOr bitor BitOrAssign bitor_assign |a, b| Self(a.0 | b.0);
    BitXor bitxor BitXorAssign bitxor_assign |a, b| Self(a.0 ^ b.0);
}

/// Formats as a sized SystemVerilog hex literal with every digit shown: `8'h5a`,
/// `9'h005`.
impl<const N: u32> fmt::Display for Bits<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = N.div_ceil(4) as usize;
        write!(f, "{N}'h{:0digits$x}", self.0)
    }
}

/// Same as [`Display`](fmt::Display), so assertion failures show the width.
impl<const N: u32> fmt::Debug for Bits<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

macro_rules! bits_radix_fmt {
    ($($trait:ident)*) => {$(
        /// Formats the unsigned value like the same trait on `u128` (flags included).
        impl<const N: u32> fmt::$trait for Bits<N> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::$trait::fmt(&self.0, f)
            }
        }
    )*};
}
bits_radix_fmt!(LowerHex UpperHex Binary);

/// `Packed` for a width whose Verilator storage is one integer (`CData` to `QData`).
macro_rules! packed_int {
    ($raw:ty => $($n:literal)*) => {$(
        impl Packed for Bits<$n> {
            const WIDTH: u32 = $n;
            type Raw = $raw;

            #[expect(clippy::cast_possible_truncation, reason = "the value has at most WIDTH bits")]
            fn to_raw(self) -> $raw {
                self.0 as $raw
            }

            fn from_raw(raw: $raw) -> Self {
                Self::new_truncating(u128::from(raw))
            }
        }
    )*};
}
packed_int!(u8 => 1 2 3 4 5 6 7 8 );
packed_int!(u16 => 9 10 11 12 13 14 15 16 );
packed_int!(u32 => 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 );
packed_int!(u64 => 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63 64 );

/// `Packed` for a width whose Verilator storage is `VlWide<W>` (65 bits and up).
macro_rules! packed_words {
    ($w:literal => $($n:literal)*) => {$(
        impl Packed for Bits<$n> {
            const WIDTH: u32 = $n;
            type Raw = [u32; $w];

            fn to_raw(self) -> [u32; $w] {
                let words = words::from_u128(self.0);
                core::array::from_fn(|i| words[i])
            }

            fn from_raw(raw: [u32; $w]) -> Self {
                Self::new_truncating(words::to_u128(&raw))
            }
        }
    )*};
}
packed_words!(3 => 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79 80 81 82 83 84 85 86 87 88 89 90 91 92 93 94 95 96 );
packed_words!(4 => 97 98 99 100 101 102 103 104 105 106 107 108 109 110 111 112 113 114 115 116 117 118 119 120 121 122 123 124 125 126 127 128 );
