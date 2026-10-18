//! Runtime support for generated value types: packed-struct field packing
//! ([`pack_field`], [`unpack_field`]), the error of a generated enum's `variant()`
//! ([`InvalidEnum`]) and one entry of a generated struct's `diff()` ([`FieldDiff`]).
//!
//! Bit `i` of a packed value is bit `i % 32` of word `i / 32` (Verilator's `VlWide` layout,
//! the [`Packed`] contract). A field of width `w` at `lsb` occupies bits
//! `lsb ..= lsb + w - 1` (IEEE 1800-2023 §7.2.1: the first declared member is the most
//! significant).

use std::fmt;

use simjig_bits::Packed;

use crate::signal::RawWords;
use crate::signal::sealed::Buffer;

/// Writes `value`'s bits into `dst` at `lsb ..= lsb + T::WIDTH - 1`, leaving every other bit
/// of `dst` unchanged.
///
/// # Panics
/// If the field does not fit in `dst`.
pub fn pack_field<T: Packed>(dst: &mut [u32], lsb: u32, value: T)
where
    T::Raw: RawWords,
{
    let mut buf = <T::Raw as Buffer>::zeroed();
    let src = buf.as_mut();
    value.to_raw().to_words(src);
    let mut done = 0;
    while done < T::WIDTH {
        let n = (T::WIDTH - done).min(32);
        insert(dst, lsb + done, n, extract(src, done, n));
        done += n;
    }
}

/// Reads a `T` from `src` at `lsb ..= lsb + T::WIDTH - 1`.
///
/// # Panics
/// If the field does not fit in `src`.
#[must_use]
pub fn unpack_field<T: Packed>(src: &[u32], lsb: u32) -> T
where
    T::Raw: RawWords,
{
    let mut buf = <T::Raw as Buffer>::zeroed();
    let dst = buf.as_mut();
    let mut done = 0;
    while done < T::WIDTH {
        let n = (T::WIDTH - done).min(32);
        insert(dst, done, n, extract(src, lsb + done, n));
        done += n;
    }
    T::from_raw(T::Raw::from_words(dst))
}

/// The `n <= 32` bits of `words` starting at bit `pos`, in the low bits of the result.
fn extract(words: &[u32], pos: u32, n: u32) -> u32 {
    let (w, o) = ((pos / 32) as usize, pos % 32);
    let low = u64::from(words[w]) >> o;
    let high = if o + n > 32 {
        u64::from(words[w + 1]) << (32 - o)
    } else {
        0
    };
    low_bits(low | high, n)
}

/// Replaces the `n <= 32` bits of `words` starting at bit `pos` with the low bits of `bits`.
fn insert(words: &mut [u32], pos: u32, n: u32, bits: u32) {
    let (w, o) = ((pos / 32) as usize, pos % 32);
    let mask = ((1u64 << n) - 1) << o;
    let value = u64::from(bits) << o & mask;
    words[w] = (words[w] & !low_bits(mask, 32)) | low_bits(value, 32);
    if o + n > 32 {
        words[w + 1] = (words[w + 1] & !low_bits(mask >> 32, 32)) | low_bits(value >> 32, 32);
    }
}

/// The low `n <= 32` bits of `value`.
fn low_bits(value: u64, n: u32) -> u32 {
    u32::try_from(value & ((1u64 << n) - 1)).unwrap_or(u32::MAX)
}

/// A value of a generated enum type that names none of its variants (SystemVerilog enums
/// may hold any value of their base type, IEEE 1800-2023 §6.19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidEnum {
    type_name: &'static str,
    width: u32,
    value: u128,
}

impl InvalidEnum {
    /// The error for `value` of the `width`-bit enum `type_name` (its SystemVerilog name).
    #[must_use]
    pub const fn new(type_name: &'static str, width: u32, value: u128) -> Self {
        Self {
            type_name,
            width,
            value,
        }
    }

    /// The enum's SystemVerilog name (`pkg::state_t`).
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// The raw value.
    #[must_use]
    pub const fn value(&self) -> u128 {
        self.value
    }
}

impl fmt::Display for InvalidEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}'h{:x} is not a value of enum `{}`",
            self.width, self.value, self.type_name
        )
    }
}

impl std::error::Error for InvalidEnum {}

/// One field that differs between two values of a generated packed struct, from its
/// `diff()`: the field's SystemVerilog path and both values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDiff {
    path: String,
    left: String,
    right: String,
}

impl FieldDiff {
    /// A difference at field `path`, with both values as their `Debug` text.
    #[must_use]
    pub fn new(path: &str, left: &dyn fmt::Debug, right: &dyn fmt::Debug) -> Self {
        Self {
            path: path.to_owned(),
            left: format!("{left:?}"),
            right: format!("{right:?}"),
        }
    }

    /// The same difference, seen from the struct that contains this one as field `parent`.
    #[must_use]
    pub fn within(self, parent: &str) -> Self {
        Self {
            path: format!("{parent}.{}", self.path),
            ..self
        }
    }

    /// The field's path (`hdr.tag`).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for FieldDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} != {}", self.path, self.left, self.right)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use simjig_bits::{Bits, WideBits};

    use super::*;

    fn bit(words: &[u32], i: u32) -> bool {
        words[(i / 32) as usize] >> (i % 32) & 1 == 1
    }

    /// Packs `Bits<N>` at every offset and checks every bit against a bitwise reference.
    fn check<const N: u32>(field: u128, dst: [u32; 7], lsb: u32)
    where
        Bits<N>: Packed<Raw: RawWords>,
    {
        let value = Bits::<N>::new_truncating(field);
        let mut out = dst;
        pack_field(&mut out, lsb, value);
        for i in 0..224 {
            let want = if (lsb..lsb + N).contains(&i) {
                field >> (i - lsb) & 1 == 1
            } else {
                bit(&dst, i)
            };
            assert_eq!(bit(&out, i), want, "bit {i}, N={N}, lsb={lsb}");
        }
        assert_eq!(unpack_field::<Bits<N>>(&out, lsb), value);
    }

    proptest! {
        #[test]
        fn pack_and_unpack_match_a_bitwise_reference(
            field: u128,
            dst: [u32; 7],
            lsb in 0u32..96,
        ) {
            check::<1>(field, dst, lsb);
            check::<7>(field, dst, lsb);
            check::<32>(field, dst, lsb);
            check::<33>(field, dst, lsb);
            check::<64>(field, dst, lsb);
            check::<65>(field, dst, lsb);
            check::<128>(field, dst, lsb);
        }

        #[test]
        fn wide_fields_round_trip(words: [u32; 7], lsb in 0u32..24) {
            let value = WideBits::<200, 7>::from_words(words);
            let mut out = [0u32; 8];
            pack_field(&mut out, lsb, value);
            prop_assert_eq!(unpack_field::<WideBits<200, 7>>(&out, lsb), value);
        }
    }

    #[test]
    fn errors_render_the_value_and_the_path() {
        assert_eq!(
            InvalidEnum::new("pkg::state_t", 2, 3).to_string(),
            "2'h3 is not a value of enum `pkg::state_t`"
        );
        let diff = FieldDiff::new("tag", &1, &2).within("hdr");
        assert_eq!(diff.to_string(), "hdr.tag: 1 != 2");
    }
}
