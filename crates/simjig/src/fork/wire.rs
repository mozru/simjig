//! How a branch's result crosses from the child process to the parent: [`BranchValue`] (the
//! values a branch may return) and the one report a child writes before it exits.
//!
//! The encoding is private to one test binary talking to itself (a parent and its forked
//! child run the same code), so it is a plain little-endian byte layout with no version:
//! integers as fixed-width little endian, lengths as `u64`, strings as UTF-8.

use simjig_bits::{Bit, Bits};

use super::RemoteFailure;
use crate::{Failure, SimTime};

/// A value a fork branch can return to the parent: it is written to bytes in the child and
/// read back in the parent.
///
/// Implemented for `()`, `bool`, the integers, `String`, [`Bit`], [`Bits`], [`SimTime`],
/// and `Vec`s, `Option`s and tuples (up to three) of those. Implement it for a type of
/// your own by encoding its fields in order.
pub trait BranchValue: Sized {
    /// Appends the value's bytes to `out`.
    fn encode(&self, out: &mut Vec<u8>);

    /// Reads a value from the front of `input` and advances it; `None` if the bytes are
    /// not a value of this type.
    fn decode(input: &mut &[u8]) -> Option<Self>;
}

/// The first `n` bytes of `input`, which then starts after them.
fn take<'a>(input: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if input.len() < n {
        return None;
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Some(head)
}

macro_rules! int_value {
    ($($t:ty),+) => {$(
        impl BranchValue for $t {
            fn encode(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }

            fn decode(input: &mut &[u8]) -> Option<Self> {
                let bytes = take(input, size_of::<$t>())?;
                Some(<$t>::from_le_bytes(bytes.try_into().ok()?))
            }
        }
    )+};
}

int_value!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128);

impl BranchValue for usize {
    fn encode(&self, out: &mut Vec<u8>) {
        (*self as u64).encode(out);
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        Self::try_from(u64::decode(input)?).ok()
    }
}

impl BranchValue for () {
    fn encode(&self, _out: &mut Vec<u8>) {}

    fn decode(_input: &mut &[u8]) -> Option<Self> {
        Some(())
    }
}

impl BranchValue for bool {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        match u8::decode(input)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl BranchValue for String {
    fn encode(&self, out: &mut Vec<u8>) {
        self.len().encode(out);
        out.extend_from_slice(self.as_bytes());
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        let len = usize::decode(input)?;
        Self::from_utf8(take(input, len)?.to_vec()).ok()
    }
}

impl<T: BranchValue> BranchValue for Vec<T> {
    fn encode(&self, out: &mut Vec<u8>) {
        self.len().encode(out);
        for item in self {
            item.encode(out);
        }
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        let len = usize::decode(input)?;
        // A length read from bad bytes must not reserve unbounded memory.
        let mut items = Self::with_capacity(len.min(input.len()));
        for _ in 0..len {
            items.push(T::decode(input)?);
        }
        Some(items)
    }
}

impl<T: BranchValue> BranchValue for Option<T> {
    fn encode(&self, out: &mut Vec<u8>) {
        self.is_some().encode(out);
        if let Some(value) = self {
            value.encode(out);
        }
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        if bool::decode(input)? {
            T::decode(input).map(Some)
        } else {
            Some(None)
        }
    }
}

impl<A: BranchValue, B: BranchValue> BranchValue for (A, B) {
    fn encode(&self, out: &mut Vec<u8>) {
        self.0.encode(out);
        self.1.encode(out);
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        Some((A::decode(input)?, B::decode(input)?))
    }
}

impl<A: BranchValue, B: BranchValue, C: BranchValue> BranchValue for (A, B, C) {
    fn encode(&self, out: &mut Vec<u8>) {
        self.0.encode(out);
        self.1.encode(out);
        self.2.encode(out);
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        Some((A::decode(input)?, B::decode(input)?, C::decode(input)?))
    }
}

impl BranchValue for Bit {
    fn encode(&self, out: &mut Vec<u8>) {
        bool::from(*self).encode(out);
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        bool::decode(input).map(Self::new)
    }
}

impl<const N: u32> BranchValue for Bits<N> {
    fn encode(&self, out: &mut Vec<u8>) {
        self.to_u128().encode(out);
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        Self::try_new(u128::decode(input)?).ok()
    }
}

impl BranchValue for SimTime {
    fn encode(&self, out: &mut Vec<u8>) {
        self.ticks().encode(out);
    }

    fn decode(input: &mut &[u8]) -> Option<Self> {
        u64::decode(input).map(SimTime)
    }
}

/// What a child reports: the branch's value, or why it failed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Report<T> {
    Value(T),
    Failed(RemoteFailure),
}

const VALUE: u8 = 0;
const FAILED: u8 = 1;

/// The bytes of a report of `value`.
pub(crate) fn value_report<T: BranchValue>(value: &T) -> Vec<u8> {
    let mut out = vec![VALUE];
    value.encode(&mut out);
    out
}

/// The bytes of a report of `failure`.
pub(crate) fn failure_report(failure: &Failure) -> Vec<u8> {
    let mut out = vec![FAILED];
    failure.kind.name().to_owned().encode(&mut out);
    failure.time.encode(&mut out);
    failure.task.encode(&mut out);
    failure.location.map(ToString::to_string).encode(&mut out);
    failure.message.encode(&mut out);
    failure.to_string().encode(&mut out);
    out
}

/// The report in `bytes`, if they hold exactly one.
pub(crate) fn decode_report<T: BranchValue>(mut bytes: &[u8]) -> Option<Report<T>> {
    let input = &mut bytes;
    let report = match u8::decode(input)? {
        VALUE => Report::Value(T::decode(input)?),
        FAILED => Report::Failed(RemoteFailure {
            kind: String::decode(input)?,
            time: SimTime::decode(input)?,
            task: Option::decode(input)?,
            location: Option::decode(input)?,
            message: String::decode(input)?,
            text: String::decode(input)?,
        }),
        _ => return None,
    };
    input.is_empty().then_some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FailureKind, Precision};

    fn round_trip<T: BranchValue + PartialEq + std::fmt::Debug>(value: T) {
        let bytes = value_report(&value);
        assert_eq!(decode_report::<T>(&bytes), Some(Report::Value(value)));
    }

    #[test]
    fn values_round_trip() {
        round_trip(());
        round_trip(true);
        round_trip(u64::MAX);
        round_trip(-5i64);
        round_trip(u128::MAX - 1);
        round_trip(usize::MAX);
        round_trip(String::from("branch \u{2713}"));
        round_trip(vec![(1u32, Some(String::from("a"))), (2, None)]);
        round_trip((Bit::ONE, Bits::<8>::new(0xa5), SimTime(77)));
        round_trip(vec![(); 3]);
    }

    #[test]
    fn a_failure_keeps_its_fields_and_text() {
        let failure = Failure {
            kind: FailureKind::TaskPanic {
                message: "boom".into(),
            },
            time: SimTime(42),
            precision: Precision::NS,
            task: Some("main".into()),
            location: None,
            message: "panicked: boom".into(),
        };
        let Some(Report::Failed(remote)) = decode_report::<()>(&failure_report(&failure)) else {
            panic!("not a failure report");
        };
        assert_eq!(remote.kind, "TaskPanic");
        assert_eq!(remote.time, SimTime(42));
        assert_eq!(remote.task.as_deref(), Some("main"));
        assert_eq!(remote.location, None);
        assert_eq!(remote.message, "panicked: boom");
        assert_eq!(remote.text, failure.to_string());
    }

    #[test]
    fn truncated_or_trailing_bytes_are_rejected() {
        let bytes = value_report(&(7u32, String::from("seven")));
        for cut in 0..bytes.len() {
            assert_eq!(
                decode_report::<(u32, String)>(&bytes[..cut]),
                None,
                "cut at {cut}"
            );
        }
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(decode_report::<(u32, String)>(&long), None);
        assert_eq!(decode_report::<bool>(&[VALUE, 2]), None);
        assert_eq!(decode_report::<()>(&[9]), None);
    }
}
