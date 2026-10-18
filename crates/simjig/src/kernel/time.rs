//! Simulation time: [`SimTime`] instants and [`SimDuration`] spans, both counted in ticks of
//! the design's time precision.
//!
//! A tick is one unit of the design's `timeprecision` (IEEE 1800 §3.14.2), as the model
//! reports it through [`Precision`]. Durations written in physical units (`ns(10)`) are
//! converted exactly: a value that is not a whole number of ticks, or that overflows `u64`,
//! is an error, never a rounded value (DECISION §2 row 8).

use std::fmt;

use super::state;

/// A physical time unit, from seconds down to femtoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimeUnit {
    /// Femtoseconds (10^-15 s).
    Fs,
    /// Picoseconds (10^-12 s).
    Ps,
    /// Nanoseconds (10^-9 s).
    Ns,
    /// Microseconds (10^-6 s).
    Us,
    /// Milliseconds (10^-3 s).
    Ms,
    /// Seconds.
    S,
}

impl TimeUnit {
    /// The unit as a power of ten of one second (`Ns` is -9).
    #[must_use]
    pub const fn exponent(self) -> i8 {
        match self {
            Self::Fs => -15,
            Self::Ps => -12,
            Self::Ns => -9,
            Self::Us => -6,
            Self::Ms => -3,
            Self::S => 0,
        }
    }

    /// The largest unit that is not larger than `10^exponent` s.
    const fn floor_of(exponent: i8) -> Self {
        match exponent {
            ..=-13 => Self::Fs,
            -12..=-10 => Self::Ps,
            -9..=-7 => Self::Ns,
            -6..=-4 => Self::Us,
            -3..=-1 => Self::Ms,
            0.. => Self::S,
        }
    }

    const fn symbol(self) -> &'static str {
        match self {
            Self::Fs => "fs",
            Self::Ps => "ps",
            Self::Ns => "ns",
            Self::Us => "us",
            Self::Ms => "ms",
            Self::S => "s",
        }
    }
}

/// The length of one tick: `10^exponent` seconds, with `exponent` in `-15..=0`.
///
/// Verilator reports this as the design's `timeprecision`; `1ps` is exponent `-12`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Precision {
    exponent: i8,
}

impl Precision {
    /// One femtosecond per tick.
    pub const FS: Self = Self { exponent: -15 };
    /// One picosecond per tick (Verilator's default precision).
    pub const PS: Self = Self { exponent: -12 };
    /// One nanosecond per tick.
    pub const NS: Self = Self { exponent: -9 };

    /// A precision of `10^exponent` seconds per tick.
    ///
    /// # Errors
    /// [`TimeError::PrecisionOutOfRange`] unless `exponent` is in `-15..=0` (1 fs to 1 s),
    /// the range IEEE 1800 allows for `timeprecision`.
    pub const fn from_exponent(exponent: i8) -> Result<Self, TimeError> {
        if exponent < -15 || exponent > 0 {
            return Err(TimeError::PrecisionOutOfRange { exponent });
        }
        Ok(Self { exponent })
    }

    /// The precision as a power of ten of one second.
    #[must_use]
    pub const fn exponent(self) -> i8 {
        self.exponent
    }

    /// Converts `value` `unit`s into a whole number of ticks.
    ///
    /// # Errors
    /// - [`TimeError::NotRepresentable`] if `value` `unit`s is not a whole number of ticks
    ///   (for example `1500 fs` at `1ps` precision);
    /// - [`TimeError::Overflow`] if the tick count does not fit in `u64`.
    pub fn duration(self, value: u64, unit: TimeUnit) -> Result<SimDuration, TimeError> {
        let shift = i32::from(unit.exponent()) - i32::from(self.exponent);
        let overflow = TimeError::Overflow { value, unit };
        if shift >= 0 {
            let factor = 10u64.checked_pow(shift.unsigned_abs()).ok_or(overflow)?;
            let ticks = value.checked_mul(factor).ok_or(overflow)?;
            Ok(SimDuration(ticks))
        } else {
            // The unit is finer than a tick: only exact multiples of a tick are allowed.
            let divisor = 10u64.pow(shift.unsigned_abs());
            if value.is_multiple_of(divisor) {
                Ok(SimDuration(value / divisor))
            } else {
                Err(TimeError::NotRepresentable {
                    value,
                    unit,
                    precision: self,
                })
            }
        }
    }

    /// The unit and multiplier that print a tick count in physical units.
    fn display_scale(self) -> (TimeUnit, u64) {
        let unit = TimeUnit::floor_of(self.exponent);
        let shift = i32::from(self.exponent) - i32::from(unit.exponent());
        (unit, 10u64.pow(shift.unsigned_abs()))
    }
}

impl fmt::Display for Precision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (unit, scale) = self.display_scale();
        write!(f, "{scale}{}", unit.symbol())
    }
}

/// An instant of simulation time, in ticks since time 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimTime(pub u64);

impl SimTime {
    /// Time zero, where every simulation starts.
    pub const ZERO: Self = Self(0);

    /// The tick count.
    #[must_use]
    pub const fn ticks(self) -> u64 {
        self.0
    }

    /// `self + duration`, or `None` on `u64` overflow.
    #[must_use]
    pub const fn checked_add(self, duration: SimDuration) -> Option<Self> {
        match self.0.checked_add(duration.0) {
            Some(t) => Some(Self(t)),
            None => None,
        }
    }

    /// A value that prints this time in physical units of `precision` (`1250 ps`).
    #[must_use]
    pub const fn display(self, precision: Precision) -> TimeDisplay {
        TimeDisplay {
            ticks: self.0,
            precision,
        }
    }
}

/// Prints a tick count in physical units; made by [`SimTime::display`].
#[derive(Clone, Copy, Debug)]
pub struct TimeDisplay {
    ticks: u64,
    precision: Precision,
}

impl fmt::Display for TimeDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (unit, scale) = self.precision.display_scale();
        let value = u128::from(self.ticks) * u128::from(scale);
        write!(f, "{value} {}", unit.symbol())
    }
}

/// A span of simulation time, in ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimDuration(pub u64);

impl SimDuration {
    /// A duration of `ticks` ticks of the design's precision. Always representable.
    #[must_use]
    pub const fn from_ticks(ticks: u64) -> Self {
        Self(ticks)
    }

    /// The tick count.
    #[must_use]
    pub const fn ticks(self) -> u64 {
        self.0
    }
}

/// Why a time value could not be built.
///
/// It may grow: a `match` outside this crate needs a wildcard arm.
///
/// ```compile_fail,E0004
/// fn code(e: &simjig::TimeError) -> u8 {
///     match e {
///         simjig::TimeError::NotRepresentable { .. } => 0,
///         simjig::TimeError::Overflow { .. } => 0,
///         simjig::TimeError::PrecisionOutOfRange { .. } => 0,
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeError {
    /// The value is not a whole number of ticks.
    NotRepresentable {
        /// The requested amount.
        value: u64,
        /// Its unit.
        unit: TimeUnit,
        /// The design's precision.
        precision: Precision,
    },
    /// The tick count does not fit in `u64`.
    Overflow {
        /// The requested amount.
        value: u64,
        /// Its unit.
        unit: TimeUnit,
    },
    /// The precision exponent is outside `-15..=0`.
    PrecisionOutOfRange {
        /// The rejected exponent.
        exponent: i8,
    },
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NotRepresentable {
                value,
                unit,
                precision,
            } => write!(
                f,
                "{value} {} is not a whole number of ticks at the design's {precision} \
                 precision; use a multiple of {precision} or raise the design's \
                 `timeprecision`",
                unit.symbol()
            ),
            Self::Overflow { value, unit } => write!(
                f,
                "{value} {} overflows the 64-bit tick counter; use a smaller duration",
                unit.symbol()
            ),
            Self::PrecisionOutOfRange { exponent } => write!(
                f,
                "time precision 1e{exponent} s is outside 1 fs ..= 1 s (exponent -15..=0)"
            ),
        }
    }
}

impl std::error::Error for TimeError {}

#[track_caller]
fn in_current(value: u64, unit: TimeUnit) -> SimDuration {
    let precision = state::with_current("a duration constructor", |k| k.precision);
    match precision.duration(value, unit) {
        Ok(d) => d,
        Err(e) => panic!("{e}"),
    }
}

/// `value` femtoseconds at the running simulation's precision.
///
/// # Panics
/// Outside [`Sim::run`](crate::Sim::run), or if the value is not a whole number of ticks
/// or overflows; use [`Precision::duration`] for a fallible conversion.
#[track_caller]
#[must_use]
pub fn fs(value: u64) -> SimDuration {
    in_current(value, TimeUnit::Fs)
}

/// `value` picoseconds at the running simulation's precision.
///
/// # Panics
/// As [`fs`].
#[track_caller]
#[must_use]
pub fn ps(value: u64) -> SimDuration {
    in_current(value, TimeUnit::Ps)
}

/// `value` nanoseconds at the running simulation's precision.
///
/// # Panics
/// As [`fs`].
#[track_caller]
#[must_use]
pub fn ns(value: u64) -> SimDuration {
    in_current(value, TimeUnit::Ns)
}

/// `value` microseconds at the running simulation's precision.
///
/// # Panics
/// As [`fs`].
#[track_caller]
#[must_use]
pub fn us(value: u64) -> SimDuration {
    in_current(value, TimeUnit::Us)
}

/// `value` milliseconds at the running simulation's precision.
///
/// # Panics
/// As [`fs`].
#[track_caller]
#[must_use]
pub fn ms(value: u64) -> SimDuration {
    in_current(value, TimeUnit::Ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(precision: Precision, value: u64, unit: TimeUnit, expect: Result<u64, TimeError>) {
        assert_eq!(
            precision.duration(value, unit).map(SimDuration::ticks),
            expect,
            "{value} {unit:?} at {precision}"
        );
    }

    #[test]
    fn durations_convert_exactly_or_fail() {
        let p = Precision::PS;
        check(p, 10, TimeUnit::Ns, Ok(10_000));
        check(p, 3, TimeUnit::Ps, Ok(3));
        check(p, 2000, TimeUnit::Fs, Ok(2));
        check(
            p,
            1500,
            TimeUnit::Fs,
            Err(TimeError::NotRepresentable {
                value: 1500,
                unit: TimeUnit::Fs,
                precision: p,
            }),
        );
        check(
            p,
            u64::MAX,
            TimeUnit::S,
            Err(TimeError::Overflow {
                value: u64::MAX,
                unit: TimeUnit::S,
            }),
        );
        let ten_ps = Precision::from_exponent(-11).unwrap();
        check(ten_ps, 30, TimeUnit::Ps, Ok(3));
        check(
            ten_ps,
            35,
            TimeUnit::Ps,
            Err(TimeError::NotRepresentable {
                value: 35,
                unit: TimeUnit::Ps,
                precision: ten_ps,
            }),
        );
    }

    #[test]
    fn precision_range_is_checked() {
        assert!(Precision::from_exponent(-16).is_err());
        assert!(Precision::from_exponent(1).is_err());
        assert_eq!(Precision::from_exponent(-11).unwrap().to_string(), "10ps");
        assert_eq!(Precision::from_exponent(-1).unwrap().to_string(), "100ms");
    }

    #[test]
    fn times_display_in_physical_units() {
        let ten_ps = Precision::from_exponent(-11).unwrap();
        assert_eq!(SimTime(125).display(ten_ps).to_string(), "1250 ps");
        assert_eq!(SimTime(7).display(Precision::NS).to_string(), "7 ns");
    }
}
