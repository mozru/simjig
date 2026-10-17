//! The error returned when an integer does not fit a bit-vector width.
//!
//! The panicking conversions (`From<u128>`, [`Bits::new`](crate::Bits::new)) format this
//! same error, so the fallible and the panicking path report identical text.

use core::error::Error;
use core::fmt;

/// An integer that does not fit in an unsigned bit vector of a given width.
///
/// Returned by [`Bits::try_new`](crate::Bits::try_new) and the `TryFrom<integer>` impls.
///
/// # Examples
///
/// ```
/// use simjig_bits::Bits;
///
/// let err = Bits::<8>::try_new(0x15a).unwrap_err();
/// assert_eq!(err.width(), 8);
/// assert_eq!(err.to_string(), "value 0x15a does not fit in 8 bits (maximum 0xff)");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutOfRange {
    value: Rejected,
    width: u32,
}

/// The rejected input. Negative inputs are kept separately because an `i128` and a `u128`
/// together cover every Rust integer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rejected {
    TooLarge(u128),
    Negative(i128),
}

impl OutOfRange {
    pub(crate) const fn too_large(value: u128, width: u32) -> Self {
        Self {
            value: Rejected::TooLarge(value),
            width,
        }
    }

    pub(crate) const fn negative(value: i128, width: u32) -> Self {
        Self {
            value: Rejected::Negative(value),
            width,
        }
    }

    /// The width, in bits, of the type the value was converted to.
    pub const fn width(&self) -> u32 {
        self.width
    }
}

impl fmt::Display for OutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self.width;
        match self.value {
            Rejected::TooLarge(value) => {
                write!(f, "value {value:#x} does not fit in {width} bits")?;
                // Every width above 128 accepts every u128, so the maximum always fits.
                if width < 128 {
                    write!(f, " (maximum {:#x})", (1u128 << width) - 1)?;
                }
                Ok(())
            }
            Rejected::Negative(value) => write!(
                f,
                "value {value} does not fit in {width} bits (the type is unsigned)"
            ),
        }
    }
}

impl Error for OutOfRange {}
