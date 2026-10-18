//! Arithmetic on little-endian `u32` word slices, the layout of Verilator's `VlWide`.
//!
//! Word `i` holds bits `32 * i + 31 ..= 32 * i`. These helpers know nothing about a
//! value's width; callers mask the top word afterwards.

use core::cmp::Ordering;

/// Splits a `u128` into four little-endian words.
pub(crate) const fn from_u128(value: u128) -> [u32; 4] {
    // Truncating casts are the point: each picks one 32-bit word.
    #[expect(clippy::cast_possible_truncation, reason = "selects one 32-bit word")]
    [
        value as u32,
        (value >> 32) as u32,
        (value >> 64) as u32,
        (value >> 96) as u32,
    ]
}

/// The low 128 bits of a word slice (fewer words are zero-extended).
pub(crate) fn to_u128(words: &[u32]) -> u128 {
    words
        .iter()
        .take(4)
        .enumerate()
        .fold(0, |acc, (i, &w)| acc | (u128::from(w) << (32 * i)))
}

/// Whether bit `index` is set.
pub(crate) fn bit(words: &[u32], index: u32) -> bool {
    (words[index as usize / 32] >> (index % 32)) & 1 == 1
}

/// Sets bit `index` to `value`.
pub(crate) fn set_bit(words: &mut [u32], index: u32, value: bool) {
    let word = &mut words[index as usize / 32];
    let shift = index % 32;
    *word = (*word & !(1 << shift)) | (u32::from(value) << shift);
}

/// Bits `lo + len - 1 ..= lo` as an integer, `len <= 128`.
pub(crate) fn extract(words: &[u32], lo: u32, len: u32) -> u128 {
    (0..len).fold(0, |acc, i| acc | (u128::from(bit(words, lo + i)) << i))
}

/// Writes the low `len <= 128` bits of `value` to bits `lo + len - 1 ..= lo`.
pub(crate) fn deposit(words: &mut [u32], lo: u32, len: u32, value: u128) {
    for i in 0..len {
        set_bit(words, lo + i, (value >> i) & 1 == 1);
    }
}

/// `a + b` with the final carry dropped.
pub(crate) fn wrapping_add<const W: usize>(a: &[u32; W], b: &[u32; W]) -> [u32; W] {
    let mut out = [0; W];
    let mut carry = false;
    for i in 0..W {
        let (sum, c1) = a[i].overflowing_add(b[i]);
        let (sum, c2) = sum.overflowing_add(u32::from(carry));
        out[i] = sum;
        carry = c1 || c2;
    }
    out
}

/// `a - b` with the final borrow dropped.
pub(crate) fn wrapping_sub<const W: usize>(a: &[u32; W], b: &[u32; W]) -> [u32; W] {
    let mut out = [0; W];
    let mut borrow = false;
    for i in 0..W {
        let (diff, b1) = a[i].overflowing_sub(b[i]);
        let (diff, b2) = diff.overflowing_sub(u32::from(borrow));
        out[i] = diff;
        borrow = b1 || b2;
    }
    out
}

/// Unsigned comparison of equal-length values: the most significant differing word
/// decides.
pub(crate) fn cmp<const W: usize>(a: &[u32; W], b: &[u32; W]) -> Ordering {
    a.iter().rev().cmp(b.iter().rev())
}

/// Exactly `count` digits of the value in base `2^bits_per_digit`, most significant
/// first. `bits_per_digit` is 1 or 4, so no digit straddles a word.
pub(crate) fn digits(words: &[u32], bits_per_digit: u32, count: u32, upper: bool) -> String {
    let table: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mask = (1 << bits_per_digit) - 1;
    (0..count)
        .rev()
        .map(|d| {
            let lo = d * bits_per_digit;
            let digit = (words[lo as usize / 32] >> (lo % 32)) & mask;
            char::from(table[digit as usize])
        })
        .collect()
}

/// The number of significant bits (0 for zero).
pub(crate) fn significant_bits(words: &[u32]) -> u32 {
    let mut bits = 0;
    for (base, &w) in (0u32..).step_by(32).zip(words) {
        if w != 0 {
            bits = base + 32 - w.leading_zeros();
        }
    }
    bits
}
