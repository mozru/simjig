//! [`Bit`]: a single 2-state bit, the value type of 1-bit signals.
//!
//! `Bit` converts from `bool` and from the integers `0` and `1`. Exactly one integer type
//! (`u128`) converts into `Bit`, so an untyped literal such as `1` infers it; see the crate
//! docs for why.

use core::fmt;
use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

use crate::error::OutOfRange;
use crate::packed::Packed;

/// A 2-state bit: `0` or `1`.
///
/// # Examples
///
/// ```
/// use simjig_bits::Bit;
///
/// let b = Bit::from(1);
/// assert_eq!(b, true);
/// assert_eq!(b, 1);
/// assert_eq!(!b, Bit::ZERO);
/// assert_eq!(b.to_string(), "1'b1");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[must_use]
pub struct Bit(bool);

impl Bit {
    /// The bit `0`.
    pub const ZERO: Self = Self(false);
    /// The bit `1`.
    pub const ONE: Self = Self(true);

    /// Makes a bit from a `bool` (`true` is `1`).
    pub const fn new(value: bool) -> Self {
        Self(value)
    }

    /// Returns `true` for `1`.
    pub const fn to_bool(self) -> bool {
        self.0
    }

    /// Converts `0` or `1`, rejecting anything else.
    ///
    /// # Examples
    ///
    /// ```
    /// use simjig_bits::Bit;
    ///
    /// assert_eq!(Bit::try_new(1), Ok(Bit::ONE));
    /// assert!(Bit::try_new(2).is_err());
    /// ```
    pub const fn try_new(value: u128) -> Result<Self, OutOfRange> {
        match value {
            0 => Ok(Self::ZERO),
            1 => Ok(Self::ONE),
            _ => Err(OutOfRange::too_large(value, 1)),
        }
    }
}

impl From<bool> for Bit {
    fn from(value: bool) -> Self {
        Self(value)
    }
}

impl From<Bit> for bool {
    fn from(value: Bit) -> Self {
        value.0
    }
}

/// Converts `0` or `1`.
///
/// # Panics
///
/// Panics at the caller's location if `value` is neither `0` nor `1`.
impl From<u128> for Bit {
    #[track_caller]
    fn from(value: u128) -> Self {
        match Self::try_new(value) {
            Ok(bit) => bit,
            Err(err) => panic!("Bit: {err}"),
        }
    }
}

macro_rules! bit_try_from_unsigned {
    ($($int:ty)*) => {$(
        impl TryFrom<$int> for Bit {
            type Error = OutOfRange;
            fn try_from(value: $int) -> Result<Self, OutOfRange> {
                Self::try_new(u128::from(value))
            }
        }
    )*};
}
bit_try_from_unsigned!(u8 u16 u32 u64);

macro_rules! bit_try_from_signed {
    ($($int:ty)*) => {$(
        impl TryFrom<$int> for Bit {
            type Error = OutOfRange;
            fn try_from(value: $int) -> Result<Self, OutOfRange> {
                match u128::try_from(value) {
                    Ok(value) => Self::try_new(value),
                    Err(_) => Err(OutOfRange::negative(i128::from(value), 1)),
                }
            }
        }
    )*};
}
bit_try_from_signed!(i8 i16 i32 i64 i128);

impl PartialEq<bool> for Bit {
    fn eq(&self, other: &bool) -> bool {
        self.0 == *other
    }
}

impl PartialEq<u128> for Bit {
    fn eq(&self, other: &u128) -> bool {
        u128::from(self.0) == *other
    }
}

/// `1 == b` as well as `b == 1`; only for `u128`, as for `Bits` (a `bool` impl would
/// break `flag == Default::default()` the same way).
impl PartialEq<Bit> for u128 {
    fn eq(&self, other: &Bit) -> bool {
        *other == *self
    }
}

impl Packed for Bit {
    const WIDTH: u32 = 1;
    type Raw = u8;

    fn to_raw(self) -> u8 {
        u8::from(self.0)
    }

    fn from_raw(raw: u8) -> Self {
        Self(raw & 1 == 1)
    }
}

impl Not for Bit {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

macro_rules! bit_binop {
    ($($trait:ident $method:ident $assign:ident $assign_method:ident $op:tt;)*) => {$(
        impl $trait for Bit {
            type Output = Self;
            fn $method(self, rhs: Self) -> Self {
                Self(self.0 $op rhs.0)
            }
        }
        impl $assign for Bit {
            fn $assign_method(&mut self, rhs: Self) {
                *self = *self $op rhs;
            }
        }
    )*};
}
bit_binop! {
    BitAnd bitand BitAndAssign bitand_assign &;
    BitOr bitor BitOrAssign bitor_assign |;
    BitXor bitxor BitXorAssign bitxor_assign ^;
}

/// Formats as a SystemVerilog literal: `1'b0` or `1'b1`.
impl fmt::Display for Bit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "1'b{}", u8::from(self.0))
    }
}

/// Same as [`Display`](fmt::Display): `1'b0` or `1'b1`.
impl fmt::Debug for Bit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::LowerHex for Bit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&u8::from(self.0), f)
    }
}

impl fmt::UpperHex for Bit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&u8::from(self.0), f)
    }
}

impl fmt::Binary for Bit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&u8::from(self.0), f)
    }
}
