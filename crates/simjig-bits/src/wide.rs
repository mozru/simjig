//! [`WideBits<N, W>`]: an unsigned 2-state bit vector of more than 128 bits, stored as
//! `W` little-endian `u32` words exactly like Verilator's `VlWide<W>`.
//!
//! `W` is redundant with `N` (`W == N.div_ceil(32)`), but stable Rust cannot compute an
//! array length from a const parameter, so the caller names both and a compile-time
//! assertion checks them. Generated code hides this behind type aliases.
//!
//! Invariant: every bit at position `N` or above (in the top word) is zero.

use core::cmp::Ordering;
use core::fmt;
use core::ops::{
    Add, AddAssign, BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not, Sub,
    SubAssign,
};

use crate::bit::Bit;
use crate::bits::{Bits, check_index, check_slice};
use crate::error::OutOfRange;
use crate::packed::Packed;
use crate::words;

/// An unsigned 2-state bit vector of `N > 128` bits in `W = N.div_ceil(32)` words.
///
/// A wrong `W`, or `N <= 128` (use [`Bits`]), fails to compile as soon as a value is
/// constructed.
///
/// # Examples
///
/// ```
/// use simjig_bits::{Bits, WideBits};
///
/// let x: WideBits<200, 7> = u128::MAX.into();
/// let y = x + WideBits::from(1);
/// assert_eq!(y.slice::<8>(135, 128), 0x01);
/// assert_eq!(y.trunc_bits::<128>(), Bits::<128>::ZERO);
/// ```
///
/// ```compile_fail,E0080
/// let _ = simjig_bits::WideBits::<200, 6>::ZERO; // 200 bits need 7 words
/// ```
///
/// ```compile_fail,E0080
/// let _ = simjig_bits::WideBits::<128, 4>::ZERO; // use Bits<128>
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[must_use]
pub struct WideBits<const N: u32, const W: usize>([u32; W]);

impl<const N: u32, const W: usize> WideBits<N, W> {
    /// The width in bits.
    pub const WIDTH: u32 = N;

    /// The valid bits of the top word. Every constructor uses it, so it also carries the
    /// compile-time checks on `N` and `W`.
    const TOP_MASK: u32 = {
        assert!(
            N > 128,
            "WideBits<N, W> needs N > 128; use Bits<N> up to 128"
        );
        assert!(
            W == N.div_ceil(32) as usize,
            "WideBits<N, W> needs W == N.div_ceil(32)"
        );
        u32::MAX >> ((32 - N % 32) % 32)
    };

    /// All bits `0`.
    pub const ZERO: Self = Self::from_words([0; W]);
    /// All bits `1`, the largest value.
    pub const MAX: Self = Self::from_words([u32::MAX; W]);

    /// Makes a value from little-endian words, ignoring bits at `N` and above.
    pub const fn from_words(mut words: [u32; W]) -> Self {
        words[W - 1] &= Self::TOP_MASK;
        Self(words)
    }

    /// The little-endian words; bits at `N` and above are zero.
    pub const fn to_words(self) -> [u32; W] {
        self.0
    }

    /// Words 0..4 from `low`, the rest all ones if `fill_ones`, else zero.
    pub(crate) fn from_u128_words(low: [u32; 4], fill_ones: bool) -> Self {
        let fill = if fill_ones { u32::MAX } else { 0 };
        Self::from_words(core::array::from_fn(|i| {
            low.get(i).copied().unwrap_or(fill)
        }))
    }

    /// Whether the value is negative read as two's complement (bit `N - 1` is set).
    fn sign(self) -> bool {
        words::bit(&self.0, N - 1)
    }

    /// The value of bit `index` (bit 0 is the least significant).
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    #[track_caller]
    pub fn bit(self, index: u32) -> Bit {
        check_index(index, N);
        Bit::new(words::bit(&self.0, index))
    }

    /// Sets bit `index` to `value`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= N`.
    #[track_caller]
    pub fn set_bit(&mut self, index: u32, value: impl Into<Bit>) {
        check_index(index, N);
        words::set_bit(&mut self.0, index, value.into().to_bool());
    }

    /// The bits `hi..=lo` (SystemVerilog `x[hi:lo]`) as an `M`-bit value, `M <= 128`.
    ///
    /// # Panics
    ///
    /// Panics unless `lo <= hi < N` and `M == hi - lo + 1`.
    #[track_caller]
    pub fn slice<const M: u32>(self, hi: u32, lo: u32) -> Bits<M> {
        check_slice(hi, lo, N, M);
        Bits::new_truncating(words::extract(&self.0, lo, M))
    }

    /// Replaces the bits `hi..=lo` (SystemVerilog `x[hi:lo] = value`), `M <= 128`.
    ///
    /// # Panics
    ///
    /// Panics unless `lo <= hi < N` and `M == hi - lo + 1`.
    #[track_caller]
    pub fn set_slice<const M: u32>(&mut self, hi: u32, lo: u32, value: Bits<M>) {
        check_slice(hi, lo, N, M);
        words::deposit(&mut self.0, lo, M, value.to_u128());
    }

    /// Zero-extends to `M >= N` bits.
    pub fn zext<const M: u32, const W2: usize>(self) -> WideBits<M, W2> {
        const { assert!(M >= N, "zext target must not be narrower than the source") };
        WideBits::from_words(core::array::from_fn(|i| {
            self.0.get(i).copied().unwrap_or(0)
        }))
    }

    /// Sign-extends to `M >= N` bits, copying bit `N - 1` into the new upper bits.
    pub fn sext<const M: u32, const W2: usize>(self) -> WideBits<M, W2> {
        const { assert!(M >= N, "sext target must not be narrower than the source") };
        let mut source = self.0;
        let fill = if self.sign() {
            source[W - 1] |= !Self::TOP_MASK;
            u32::MAX
        } else {
            0
        };
        WideBits::from_words(core::array::from_fn(|i| {
            source.get(i).copied().unwrap_or(fill)
        }))
    }

    /// Keeps the low `M <= N` bits, `M > 128`. Use [`trunc_bits`](Self::trunc_bits) for
    /// a [`Bits`] result.
    pub fn trunc<const M: u32, const W2: usize>(self) -> WideBits<M, W2> {
        const { assert!(M <= N, "trunc target must not be wider than the source") };
        WideBits::from_words(core::array::from_fn(|i| self.0[i]))
    }

    /// Keeps the low `M <= 128` bits.
    pub fn trunc_bits<const M: u32>(self) -> Bits<M> {
        Bits::new_truncating(words::to_u128(&self.0))
    }

    /// `self + rhs`, wrapping at `N` bits (SystemVerilog `+` at equal widths).
    pub fn wrapping_add(self, rhs: Self) -> Self {
        Self::from_words(words::wrapping_add(&self.0, &rhs.0))
    }

    /// `self - rhs`, wrapping at `N` bits (SystemVerilog `-` at equal widths).
    pub fn wrapping_sub(self, rhs: Self) -> Self {
        Self::from_words(words::wrapping_sub(&self.0, &rhs.0))
    }
}

impl<const N: u32, const W: usize> Default for WideBits<N, W> {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Zero-extends an integer, typically a literal. Every `u128` fits, so this never fails.
impl<const N: u32, const W: usize> From<u128> for WideBits<N, W> {
    fn from(value: u128) -> Self {
        Self::from_u128_words(words::from_u128(value), false)
    }
}

macro_rules! wide_try_from {
    ($($int:ty)*) => {$(
        /// Fails only for negative values.
        impl<const N: u32, const W: usize> TryFrom<$int> for WideBits<N, W> {
            type Error = OutOfRange;
            fn try_from(value: $int) -> Result<Self, OutOfRange> {
                match u128::try_from(value) {
                    Ok(value) => Ok(Self::from(value)),
                    Err(_) => Err(OutOfRange::negative(i128::from(value), N)),
                }
            }
        }
    )*};
}
wide_try_from!(u8 u16 u32 u64 i8 i16 i32 i64 i128);

impl<const N: u32, const W: usize> PartialEq<u128> for WideBits<N, W> {
    fn eq(&self, other: &u128) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}

/// `4 == x` as well as `x == 4`; only for `u128`, as for `Bits`.
impl<const N: u32, const W: usize> PartialEq<WideBits<N, W>> for u128 {
    fn eq(&self, other: &WideBits<N, W>) -> bool {
        *other == *self
    }
}

impl<const N: u32, const W: usize> PartialOrd<u128> for WideBits<N, W> {
    fn partial_cmp(&self, other: &u128) -> Option<Ordering> {
        Some(self.cmp(&Self::from(*other)))
    }
}

/// Unsigned numeric order.
impl<const N: u32, const W: usize> Ord for WideBits<N, W> {
    fn cmp(&self, other: &Self) -> Ordering {
        words::cmp(&self.0, &other.0)
    }
}

impl<const N: u32, const W: usize> PartialOrd for WideBits<N, W> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<const N: u32, const W: usize> Not for WideBits<N, W> {
    type Output = Self;
    fn not(self) -> Self {
        Self::from_words(self.0.map(|w| !w))
    }
}

macro_rules! wide_binop {
    ($($trait:ident $method:ident $assign:ident $assign_method:ident |$a:ident, $b:ident| $body:expr;)*) => {$(
        impl<const N: u32, const W: usize> $trait for WideBits<N, W> {
            type Output = Self;
            fn $method(self, rhs: Self) -> Self {
                let ($a, $b) = (self, rhs);
                $body
            }
        }
        impl<const N: u32, const W: usize> $assign for WideBits<N, W> {
            fn $assign_method(&mut self, rhs: Self) {
                *self = $trait::$method(*self, rhs);
            }
        }
    )*};
}
wide_binop! {
    Add add AddAssign add_assign |a, b| a.wrapping_add(b);
    Sub sub SubAssign sub_assign |a, b| a.wrapping_sub(b);
    BitAnd bitand BitAndAssign bitand_assign |a, b| Self(core::array::from_fn(|i| a.0[i] & b.0[i]));
    BitOr bitor BitOrAssign bitor_assign |a, b| Self(core::array::from_fn(|i| a.0[i] | b.0[i]));
    BitXor bitxor BitXorAssign bitxor_assign |a, b| Self(core::array::from_fn(|i| a.0[i] ^ b.0[i]));
}

impl<const N: u32, const W: usize> Packed for WideBits<N, W> {
    const WIDTH: u32 = N;
    type Raw = [u32; W];

    fn to_raw(self) -> [u32; W] {
        self.0
    }

    fn from_raw(raw: [u32; W]) -> Self {
        Self::from_words(raw)
    }
}

/// Formats as a sized SystemVerilog hex literal with every digit shown: `200'h00ff...`.
impl<const N: u32, const W: usize> fmt::Display for WideBits<N, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{N}'h{}",
            words::digits(&self.0, 4, N.div_ceil(4), false)
        )
    }
}

/// Same as [`Display`](fmt::Display), so assertion failures show the width.
impl<const N: u32, const W: usize> fmt::Debug for WideBits<N, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Writes the unsigned value in base `2^bits_per_digit` like the integer formatters:
/// no leading zeros, honouring width, fill, `#` and `0` flags.
fn fmt_radix(
    words: &[u32],
    f: &mut fmt::Formatter<'_>,
    bits_per_digit: u32,
    prefix: &str,
    upper: bool,
) -> fmt::Result {
    let count = words::significant_bits(words)
        .div_ceil(bits_per_digit)
        .max(1);
    f.pad_integral(
        true,
        prefix,
        &words::digits(words, bits_per_digit, count, upper),
    )
}

impl<const N: u32, const W: usize> fmt::LowerHex for WideBits<N, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_radix(&self.0, f, 4, "0x", false)
    }
}

impl<const N: u32, const W: usize> fmt::UpperHex for WideBits<N, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_radix(&self.0, f, 4, "0x", true)
    }
}

impl<const N: u32, const W: usize> fmt::Binary for WideBits<N, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_radix(&self.0, f, 1, "0b", false)
    }
}
