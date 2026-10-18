//! Canonical integral literals and the SystemVerilog sized-literal parser.
//!
//! A [`Literal`] is a two-state bit vector: a width, a signedness flag and the value as
//! little-endian 64-bit words. The canonical form has exactly `ceil(width / 64)` words and
//! every bit above `width` is zero, so two literals are equal exactly when they denote the
//! same typed value.
//!
//! The parser accepts IEEE 1800-2023 §5.7.1 sized based literals (`8'hA5`, `32'sd7`,
//! `4'b1x0z` is rejected), which is the form Verilator prints for every constant. X and Z
//! digits are rejected until the IR grows four-state values. A value that does not fit its
//! width is an error rather than being truncated (§5.7.1 truncates with a warning; here a
//! silent truncation would hide a front-end bug).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A sized two-state integral value in canonical form.
///
/// # Examples
/// ```
/// use svtypes::literal::Literal;
///
/// let l: Literal = "70'h3_0000_0000_0000_0001".parse()?;
/// assert_eq!((l.width(), l.words()), (70, &[1u64, 3][..]));
/// assert_eq!(l.to_string(), "70'h30000000000000001");
/// # Ok::<(), svtypes::literal::LiteralError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawLiteral")]
pub struct Literal {
    width: u32,
    signed: bool,
    words: Vec<u64>,
}

/// The unchecked serialized form of a [`Literal`].
#[derive(Deserialize)]
struct RawLiteral {
    width: u32,
    signed: bool,
    words: Vec<u64>,
}

impl TryFrom<RawLiteral> for Literal {
    type Error = LiteralError;
    fn try_from(raw: RawLiteral) -> Result<Self, LiteralError> {
        Literal::new(raw.width, raw.signed, raw.words)
    }
}

/// Why a literal is malformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteralError {
    /// The rejected input (the source text, or a description of the words).
    pub input: String,
    /// What is wrong with it.
    pub reason: String,
}

impl fmt::Display for LiteralError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid literal `{}`: {}", self.input, self.reason)
    }
}

impl std::error::Error for LiteralError {}

fn word_count(width: u32) -> usize {
    width.div_ceil(64) as usize
}

impl Literal {
    /// Builds a literal from little-endian words.
    ///
    /// # Errors
    /// Fails when `width` is 0, when `words.len()` is not `ceil(width / 64)`, or when a bit
    /// at or above `width` is set.
    pub fn new(width: u32, signed: bool, words: Vec<u64>) -> Result<Self, LiteralError> {
        let err = |reason: String| LiteralError {
            input: format!("width {width}, words {words:x?}"),
            reason,
        };
        if width == 0 {
            return Err(err("width must be at least 1".into()));
        }
        if words.len() != word_count(width) {
            return Err(err(format!("expected {} words", word_count(width))));
        }
        if !fits(&words, width) {
            return Err(err(format!("value does not fit in {width} bits")));
        }
        Ok(Literal {
            width,
            signed,
            words,
        })
    }

    /// Builds a literal from a `u128`.
    ///
    /// # Errors
    /// Fails when `width` is 0 or `value` does not fit in `width` bits.
    pub fn from_u128(width: u32, signed: bool, value: u128) -> Result<Self, LiteralError> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "splitting a u128 into its two 64-bit halves"
        )]
        let mut words = vec![value as u64, (value >> 64) as u64];
        let needed = word_count(width).max(1);
        if words[needed.min(2)..].iter().any(|&w| w != 0) {
            return Err(LiteralError {
                input: format!("{value:#x}"),
                reason: format!("value does not fit in {width} bits"),
            });
        }
        words.resize(needed, 0);
        Literal::new(width, signed, words)
    }

    /// Width in bits.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Whether the value is to be read as two's complement.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        self.signed
    }

    /// The value as little-endian 64-bit words; bits above the width are zero.
    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// The raw bits as an unsigned number, if they fit in a `u64`.
    #[must_use]
    pub fn to_u64(&self) -> Option<u64> {
        match self.words.as_slice() {
            [w] => Some(*w),
            [w, rest @ ..] if rest.iter().all(|&r| r == 0) => Some(*w),
            _ => None,
        }
    }

    /// The same bits reinterpreted with another width and signedness.
    ///
    /// # Errors
    /// Fails when the value does not fit in `width` bits.
    pub fn retyped(&self, width: u32, signed: bool) -> Result<Self, LiteralError> {
        let mut words = self.words.clone();
        let needed = word_count(width);
        if words.len() > needed {
            if words[needed..].iter().any(|&w| w != 0) {
                return Err(LiteralError {
                    input: self.to_string(),
                    reason: format!("value does not fit in {width} bits"),
                });
            }
            words.truncate(needed);
        }
        words.resize(needed, 0);
        Literal::new(width, signed, words).map_err(|e| LiteralError {
            input: self.to_string(),
            reason: e.reason,
        })
    }
}

/// Whether no bit at or above `width` is set.
fn fits(words: &[u64], width: u32) -> bool {
    let full = (width / 64) as usize;
    let rem = width % 64;
    words.iter().enumerate().all(|(i, &w)| {
        if i < full {
            true
        } else if i == full && rem != 0 {
            w >> rem == 0
        } else {
            w == 0
        }
    })
}

impl fmt::Display for Literal {
    /// Canonical SystemVerilog spelling: `<width>'[s]h<hex>`, lower case, no separators.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.signed { "s" } else { "" };
        write!(f, "{}'{sign}h", self.width)?;
        let mut words = self.words.iter().rev().skip_while(|&&w| w == 0);
        match words.next() {
            None => write!(f, "0"),
            Some(top) => {
                write!(f, "{top:x}")?;
                words.try_for_each(|w| write!(f, "{w:016x}"))
            }
        }
    }
}

impl FromStr for Literal {
    type Err = LiteralError;

    /// Parses `<width>'[s|S]<b|o|d|h><digits>`; `_` separators are allowed in the digits.
    fn from_str(input: &str) -> Result<Self, LiteralError> {
        let err = |reason: &str| LiteralError {
            input: input.to_owned(),
            reason: reason.to_owned(),
        };
        let (width_text, rest) = input
            .split_once('\'')
            .ok_or_else(|| err("expected a sized literal such as 8'hff"))?;
        let width: u32 = width_text
            .trim()
            .parse()
            .map_err(|e| err(&format!("width is not a decimal number ({e})")))?;
        if width == 0 {
            return Err(err("width must be at least 1"));
        }
        let (signed, rest) = match rest.strip_prefix(['s', 'S']) {
            Some(r) => (true, r),
            None => (false, rest),
        };
        let mut chars = rest.chars();
        let radix = match chars.next().map(|c| c.to_ascii_lowercase()) {
            Some('b') => 2,
            Some('o') => 8,
            Some('d') => 10,
            Some('h') => 16,
            _ => return Err(err("expected base b, o, d or h after '")),
        };
        let digits = chars.as_str().trim();
        let words = parse_digits(digits, radix, width).map_err(|reason| err(&reason))?;
        Literal::new(width, signed, words).map_err(|e| err(&e.reason))
    }
}

/// Accumulates `digits` in `radix` into `ceil(width / 64)` little-endian words.
fn parse_digits(digits: &str, radix: u32, width: u32) -> Result<Vec<u64>, String> {
    let mut words = vec![0u64; word_count(width)];
    let mut any = false;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        if matches!(c.to_ascii_lowercase(), 'x' | 'z' | '?') {
            return Err("X and Z digits are not supported yet".into());
        }
        let digit = c
            .to_digit(radix)
            .ok_or_else(|| format!("`{c}` is not a base-{radix} digit"))?;
        if !mul_add(&mut words, u64::from(radix), u64::from(digit)) || !fits(&words, width) {
            return Err(format!("value does not fit in {width} bits"));
        }
        any = true;
    }
    if !any {
        return Err("no digits".into());
    }
    Ok(words)
}

/// `words = words * mul + add`; returns false on overflow out of the top word.
fn mul_add(words: &mut [u64], mul: u64, add: u64) -> bool {
    let mut carry = u128::from(add);
    for w in words.iter_mut() {
        let v = u128::from(*w) * u128::from(mul) + carry;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "keeping the low 64 bits; the high bits are carried"
        )]
        {
            *w = v as u64;
        }
        carry = v >> 64;
    }
    carry == 0
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn lit(s: &str) -> Literal {
        s.parse().unwrap()
    }

    #[test]
    fn parses_verilator_spellings() {
        let cases: &[(&str, u32, bool, &[u64])] = &[
            ("32'sh3", 32, true, &[3]),
            ("4'h5", 4, false, &[5]),
            ("8'ha5", 8, false, &[0xa5]),
            ("8'HA5", 8, false, &[0xa5]),
            ("100'h0", 100, false, &[0, 0]),
            ("70'h30000000000000001", 70, false, &[1, 3]),
            ("32'hfffffffd", 32, false, &[0xffff_fffd]),
            ("1'b1", 1, false, &[1]),
            ("6'o77", 6, false, &[63]),
            ("16'd65_535", 16, false, &[65535]),
            ("3'Sb101", 3, true, &[5]),
            (
                "128'hffffffffffffffffffffffffffffffff",
                128,
                false,
                &[u64::MAX, u64::MAX],
            ),
        ];
        for &(text, width, signed, words) in cases {
            let l = lit(text);
            assert_eq!(
                (l.width(), l.is_signed(), l.words()),
                (width, signed, words),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_malformed() {
        let cases = [
            ("8'hxx", "X and Z"),
            ("4'bz", "X and Z"),
            ("4'b?", "X and Z"),
            ("4'h10", "does not fit"),
            ("65'h20000000000000000", "does not fit"),
            ("0'h0", "at least 1"),
            ("ff", "sized literal"),
            ("8'q1", "base"),
            ("8'h", "no digits"),
            ("8'b102", "base-2 digit"),
            ("x'h1", "decimal"),
        ];
        for (text, reason) in cases {
            let e = text.parse::<Literal>().unwrap_err();
            assert!(e.reason.contains(reason), "{text}: {e}");
        }
    }

    #[test]
    fn displays_canonically() {
        assert_eq!(lit("8'd165").to_string(), "8'ha5");
        assert_eq!(lit("100'h0").to_string(), "100'h0");
        assert_eq!(lit("32'sd5").to_string(), "32'sh5");
        assert_eq!(
            lit("70'h30000000000000001").to_string(),
            "70'h30000000000000001"
        );
    }

    #[test]
    fn new_enforces_canonical_form() {
        assert!(Literal::new(8, false, vec![0x100]).is_err());
        assert!(Literal::new(64, false, vec![1, 0]).is_err());
        assert!(Literal::new(65, false, vec![1, 2]).is_err());
        assert!(Literal::new(65, false, vec![1, 1]).is_ok());
        let json = r#"{"width":4,"signed":false,"words":[16]}"#;
        assert!(serde_json::from_str::<Literal>(json).is_err());
    }

    #[test]
    fn retyped_checks_fit() {
        let l = lit("32'h5");
        assert_eq!(l.retyped(4, true).unwrap(), lit("4'sh5"));
        assert!(l.retyped(2, false).is_err());
        assert_eq!(lit("70'h1").retyped(8, false).unwrap(), lit("8'h1"));
        assert_eq!(
            lit("8'hff").retyped(100, false).unwrap().words(),
            &[0xff, 0]
        );
    }

    fn mask(value: u128, width: u32) -> u128 {
        if width >= 128 {
            value
        } else {
            value & ((1u128 << width) - 1)
        }
    }

    proptest! {
        #[test]
        fn hex_decimal_binary_agree(raw in any::<u128>(), width in 1u32..=128, signed: bool) {
            let v = mask(raw, width);
            let s = if signed { "s" } else { "" };
            let hex = lit(&format!("{width}'{s}h{v:x}"));
            prop_assert_eq!(&hex, &lit(&format!("{width}'{s}d{v}")));
            prop_assert_eq!(&hex, &lit(&format!("{width}'{s}b{v:b}")));
            prop_assert_eq!(&hex, &lit(&format!("{width}'{s}o{v:o}")));
            prop_assert_eq!(&hex, &Literal::from_u128(width, signed, v).unwrap());
        }

        #[test]
        fn display_round_trips(raw in any::<u128>(), width in 1u32..=200, signed: bool) {
            let v = mask(raw, width);
            let l = Literal::from_u128(width, signed, v).unwrap();
            prop_assert_eq!(&l.to_string().parse::<Literal>().unwrap(), &l);
            let json = serde_json::to_string(&l).unwrap();
            prop_assert_eq!(&serde_json::from_str::<Literal>(&json).unwrap(), &l);
        }

        #[test]
        fn overflow_is_rejected(width in 1u32..128, extra in 0u32..8) {
            // 2^(width + extra) never fits in `width` bits.
            let v = 1u128 << (width + extra).min(127);
            prop_assume!(v >> width != 0);
            let hex = format!("{width}'h{v:x}");
            let dec = format!("{width}'d{v}");
            prop_assert!(hex.parse::<Literal>().is_err());
            prop_assert!(dec.parse::<Literal>().is_err());
            prop_assert!(Literal::from_u128(width, false, v).is_err());
        }

        #[test]
        fn to_u64_matches_value(raw in any::<u128>(), width in 1u32..=128) {
            let v = mask(raw, width);
            let l = Literal::from_u128(width, false, v).unwrap();
            prop_assert_eq!(l.to_u64(), u64::try_from(v).ok());
        }
    }
}
