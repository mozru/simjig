//! The width calculator: packed widths, ranges and C++ storage, computed from structure.
//!
//! Producers' own width fields are never trusted (Verilator's JSON has none anyway): every
//! width in the IR comes from these functions. All arithmetic is checked, so a pathological
//! design yields a [`WidthError`] instead of a wrapped width.
//!
//! Rules (IEEE 1800-2023): a range `[l:r]` has `|l - r| + 1` elements (§7.4); a packed
//! struct is the sum of its fields, first field most significant (§7.2.1); a packed union's
//! fields all have the same width (§7.3.1); a packed array is element width times length
//! (§7.4.1).

use std::fmt;

use crate::ir::{Range, Storage};

/// A width that cannot be computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidthError(pub String);

impl fmt::Display for WidthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WidthError {}

impl Range {
    /// Number of elements, `|left - right| + 1`.
    ///
    /// # Errors
    /// Fails when the count does not fit in a `u32`.
    pub fn count(self) -> Result<u32, WidthError> {
        let span = (i64::from(self.left) - i64::from(self.right)).unsigned_abs() + 1;
        u32::try_from(span).map_err(|e| WidthError(format!("range {self} is too large: {e}")))
    }

    /// Whether `left < right` (`[0:7]`).
    #[must_use]
    pub fn ascending(self) -> bool {
        self.left < self.right
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}:{}]", self.left, self.right)
    }
}

impl std::str::FromStr for Range {
    type Err = WidthError;

    /// Parses `l:r` or `[l:r]` with decimal, possibly negative, bounds.
    fn from_str(text: &str) -> Result<Self, WidthError> {
        let err = || WidthError(format!("malformed range `{text}`"));
        let inner = text.trim();
        let inner = match inner.strip_prefix('[') {
            Some(rest) => rest.strip_suffix(']').ok_or_else(err)?,
            None => inner,
        };
        let (left, right) = inner.split_once(':').ok_or_else(err)?;
        let bound = |s: &str| {
            s.trim()
                .parse::<i32>()
                .map_err(|e| WidthError(format!("malformed range `{text}`: {e}")))
        };
        Ok(Range {
            left: bound(left)?,
            right: bound(right)?,
        })
    }
}

/// Width of a packed struct: the sum of its field widths.
///
/// # Errors
/// Fails on an empty struct or when the sum overflows.
pub fn struct_width(fields: &[u32]) -> Result<u32, WidthError> {
    if fields.is_empty() {
        return Err(WidthError("packed struct has no fields".into()));
    }
    fields.iter().try_fold(0u32, |acc, &w| {
        acc.checked_add(w)
            .ok_or_else(|| WidthError("packed struct width overflows u32".into()))
    })
}

/// Least significant bit of each field of a packed struct, first field most significant.
///
/// # Errors
/// Fails like [`struct_width`].
pub fn struct_lsbs(fields: &[u32]) -> Result<Vec<u32>, WidthError> {
    struct_width(fields)?;
    let mut lsb = 0u32;
    let mut out: Vec<u32> = fields
        .iter()
        .rev()
        .map(|&w| {
            let this = lsb;
            lsb += w; // Cannot overflow: the total was checked above.
            this
        })
        .collect();
    out.reverse();
    Ok(out)
}

/// Width of a packed union: the common width of its fields.
///
/// # Errors
/// Fails on an empty union or on fields of different widths.
pub fn union_width(fields: &[u32]) -> Result<u32, WidthError> {
    let (&first, rest) = fields
        .split_first()
        .ok_or_else(|| WidthError("packed union has no fields".into()))?;
    if rest.iter().any(|&w| w != first) {
        return Err(WidthError(format!(
            "packed union fields differ in width ({fields:?}); IEEE 1800-2023 §7.3.1 requires equal widths"
        )));
    }
    Ok(first)
}

/// Width of a packed array: element width times length.
///
/// # Errors
/// Fails when the range or the product overflows.
pub fn packed_array_width(elem: u32, range: Range) -> Result<u32, WidthError> {
    elem.checked_mul(range.count()?).ok_or_else(|| {
        WidthError(format!(
            "packed array {range} of {elem}-bit elements overflows u32"
        ))
    })
}

/// Verilator's C++ storage for a packed value of `width` bits.
///
/// # Errors
/// Fails on width 0.
pub fn packed_storage(width: u32) -> Result<Storage, WidthError> {
    Ok(match width {
        0 => return Err(WidthError("zero-width value has no storage".into())),
        1..=8 => Storage::CData,
        9..=16 => Storage::SData,
        17..=32 => Storage::IData,
        33..=64 => Storage::QData,
        _ => Storage::VlWide {
            words: width.div_ceil(32),
        },
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn range_parsing() {
        let cases = [
            ("7:0", 7, 0, 8),
            ("[0:7]", 0, 7, 8),
            ("3:-2", 3, -2, 6),
            ("[2:-1]", 2, -1, 4),
            (" [5:5] ", 5, 5, 1),
        ];
        for (text, left, right, len) in cases {
            let r: Range = text.parse().unwrap();
            assert_eq!(
                (r.left, r.right, r.count().unwrap()),
                (left, right, len),
                "{text}"
            );
        }
        for bad in ["7", "[7:0", "a:0", "7:0]", ""] {
            assert!(bad.parse::<Range>().is_err(), "{bad}");
        }
    }

    #[test]
    fn storage_thresholds_match_verilator_headers() {
        // From V*.h generated by Verilator 5.052 (see docs/notes/svtypes.md).
        let cases = [
            (1, Storage::CData),
            (8, Storage::CData),
            (9, Storage::SData),
            (16, Storage::SData),
            (17, Storage::IData),
            (32, Storage::IData),
            (33, Storage::QData),
            (64, Storage::QData),
            (65, Storage::VlWide { words: 3 }),
            (96, Storage::VlWide { words: 3 }),
            (97, Storage::VlWide { words: 4 }),
            (100, Storage::VlWide { words: 4 }),
        ];
        for (w, s) in cases {
            assert_eq!(packed_storage(w).unwrap(), s, "{w}");
        }
        assert!(packed_storage(0).is_err());
    }

    #[test]
    fn aggregates() {
        // pkg::inner_t from the struct fixture: logic [3:0] a; logic signed [11:0] b.
        assert_eq!(struct_width(&[4, 12]).unwrap(), 16);
        assert_eq!(struct_lsbs(&[4, 12]).unwrap(), [12, 0]);
        assert_eq!(struct_lsbs(&[16, 2, 1]).unwrap(), [3, 1, 0]);
        assert!(struct_width(&[]).is_err());
        assert!(struct_width(&[u32::MAX, 1]).is_err());
        assert_eq!(union_width(&[16, 16]).unwrap(), 16);
        assert!(union_width(&[16, 8]).is_err());
        let r = Range { left: 3, right: 0 };
        assert_eq!(packed_array_width(3, r).unwrap(), 12);
        assert!(packed_array_width(u32::MAX, r).is_err());
        let huge = Range {
            left: i32::MAX,
            right: i32::MIN,
        };
        assert!(huge.count().is_err());
    }

    proptest! {
        #[test]
        fn range_len_is_span_plus_one(left: i16, right: i16) {
            let r = Range { left: left.into(), right: right.into() };
            let expect = (i64::from(left) - i64::from(right)).abs() + 1;
            prop_assert_eq!(i64::from(r.count().unwrap()), expect);
            prop_assert_eq!(r.to_string().parse::<Range>().unwrap(), r);
            prop_assert_eq!(r.ascending(), left < right);
        }

        #[test]
        fn struct_layout_tiles_the_width(fields in prop::collection::vec(1u32..200, 1..12)) {
            let total = struct_width(&fields).unwrap();
            prop_assert_eq!(u64::from(total), fields.iter().map(|&w| u64::from(w)).sum::<u64>());
            let lsbs = struct_lsbs(&fields).unwrap();
            // Fields are contiguous, last field at bit 0, first field ends at the top bit.
            prop_assert_eq!(*lsbs.last().unwrap(), 0);
            prop_assert_eq!(lsbs[0] + fields[0], total);
            for i in 1..fields.len() {
                prop_assert_eq!(lsbs[i] + fields[i], lsbs[i - 1]);
            }
        }

        #[test]
        fn packed_array_matches_u64_math(elem in 1u32..100_000, left: i16, right: i16) {
            let r = Range { left: left.into(), right: right.into() };
            let exact = u64::from(elem) * u64::from(r.count().unwrap());
            match packed_array_width(elem, r) {
                Ok(w) => prop_assert_eq!(u64::from(w), exact),
                Err(_) => prop_assert!(exact > u64::from(u32::MAX)),
            }
        }

        #[test]
        fn storage_word_count(width in 65u32..10_000) {
            let Storage::VlWide { words } = packed_storage(width).unwrap() else {
                panic!("expected VlWide");
            };
            prop_assert!(words * 32 >= width && (words - 1) * 32 < width);
        }
    }
}
