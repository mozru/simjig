//! Kernel-owned clocks: clocks are data, not tasks (DECISION §1).
//!
//! Each clock is a schedule of edges. At a timestep, every clock with an edge due writes
//! its new level into the write log (writer: the clock) in declaration order, and all of
//! them share the timestep's first eval. Coincident edges of different clocks therefore
//! land in one eval, which is what clock-domain-crossing designs need.
//!
//! A clock starts low; its first rising edge is at `phase` (default 0), it stays high for
//! `high` ticks (default half the period), and it repeats every `period` ticks.

use std::fmt;

use super::signal::RawSig;
use super::time::{SimDuration, SimTime};

/// A clock to be driven by the kernel; add it with [`Sim::add_clock`](crate::Sim::add_clock).
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    sig: RawSig,
    period: SimDuration,
    high: Option<SimDuration>,
    phase: SimDuration,
}

impl Clock {
    /// A 50 % duty-cycle clock on `sig` with its first rising edge at time 0.
    #[must_use]
    pub const fn new(sig: RawSig, period: SimDuration) -> Self {
        Self {
            sig,
            period,
            high: None,
            phase: SimDuration(0),
        }
    }

    /// Sets how long the clock stays high in each period.
    #[must_use]
    pub const fn high_for(mut self, high: SimDuration) -> Self {
        self.high = Some(high);
        self
    }

    /// Delays the first rising edge to `phase`; the clock is low before it.
    #[must_use]
    pub const fn first_rise_at(mut self, phase: SimDuration) -> Self {
        self.phase = phase;
        self
    }

    pub(crate) const fn sig(&self) -> RawSig {
        self.sig
    }
}

/// Why a clock was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClockError {
    /// Clocks drive 1-bit signals only.
    NotOneBit {
        /// The signal.
        signal: String,
        /// Its width.
        width: u32,
    },
    /// The signal already has a clock.
    AlreadyClocked {
        /// The signal.
        signal: String,
    },
    /// The high time must be at least one tick and less than the period.
    BadHighTime {
        /// The signal.
        signal: String,
        /// Period in ticks.
        period: u64,
        /// High time in ticks.
        high: u64,
    },
    /// No high time was given and the period has an odd number of ticks.
    OddPeriod {
        /// The signal.
        signal: String,
        /// Period in ticks.
        period: u64,
    },
}

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOneBit { signal, width } => {
                write!(
                    f,
                    "cannot clock `{signal}`: it is {width} bits wide, clocks drive 1-bit signals"
                )
            }
            Self::AlreadyClocked { signal } => write!(f, "`{signal}` already has a clock"),
            Self::BadHighTime {
                signal,
                period,
                high,
            } => write!(
                f,
                "clock on `{signal}`: high time {high} ticks must be in 1..{period} (the period)"
            ),
            Self::OddPeriod { signal, period } => write!(
                f,
                "clock on `{signal}`: a {period}-tick period cannot be split 50/50; \
                 set the high time with Clock::high_for"
            ),
        }
    }
}

impl std::error::Error for ClockError {}

/// A validated clock and its next edge.
#[derive(Debug)]
pub(crate) struct ClockState {
    pub(crate) slot: u32,
    period: u64,
    high: u64,
    pub(crate) next_time: SimTime,
    next_level: u32,
    /// The delayed first rise, pending while the clock sits at its initial low.
    first_rise: Option<u64>,
}

impl ClockState {
    /// Validates `clock`; `width` and `name` describe its signal.
    pub(crate) fn new(clock: &Clock, width: u32, name: &str) -> Result<Self, ClockError> {
        let signal = name.to_owned();
        if width != 1 {
            return Err(ClockError::NotOneBit { signal, width });
        }
        let period = clock.period.ticks();
        let high = match clock.high {
            Some(h) => h.ticks(),
            None if period.is_multiple_of(2) => period / 2,
            None => return Err(ClockError::OddPeriod { signal, period }),
        };
        if high == 0 || high >= period {
            return Err(ClockError::BadHighTime {
                signal,
                period,
                high,
            });
        }
        let phase = clock.phase.ticks();
        Ok(Self {
            slot: clock.sig.slot(),
            period,
            high,
            next_time: SimTime::ZERO,
            next_level: u32::from(phase == 0),
            first_rise: (phase != 0).then_some(phase),
        })
    }

    /// Returns the level due at `next_time` and schedules the edge after it.
    pub(crate) fn take_edge(&mut self) -> u32 {
        let level = self.next_level;
        let t = self.next_time.ticks();
        let next = match (level, self.first_rise.take()) {
            (0, Some(first)) => first,
            (1, _) => t.saturating_add(self.high),
            _ => t.saturating_add(self.period - self.high),
        };
        self.next_time = SimTime(next);
        self.next_level = 1 - level;
        level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(clock: &Clock, n: usize) -> Vec<(u64, u32)> {
        let mut state = ClockState::new(clock, 1, "clk").unwrap();
        (0..n)
            .map(|_| {
                let t = state.next_time.ticks();
                (t, state.take_edge())
            })
            .collect()
    }

    #[test]
    fn edge_schedules_follow_period_high_and_phase() {
        let sig = RawSig::new(0, 0);
        let plain = Clock::new(sig, SimDuration(10));
        assert_eq!(edges(&plain, 4), [(0, 1), (5, 0), (10, 1), (15, 0)]);
        let shaped = plain.high_for(SimDuration(3)).first_rise_at(SimDuration(4));
        assert_eq!(
            edges(&shaped, 5),
            [(0, 0), (4, 1), (7, 0), (14, 1), (17, 0)]
        );
    }

    #[test]
    fn invalid_clocks_are_rejected() {
        let sig = RawSig::new(0, 0);
        let odd = Clock::new(sig, SimDuration(7));
        assert!(matches!(
            ClockState::new(&odd, 1, "c"),
            Err(ClockError::OddPeriod { .. })
        ));
        let flat = Clock::new(sig, SimDuration(4)).high_for(SimDuration(4));
        assert!(matches!(
            ClockState::new(&flat, 1, "c"),
            Err(ClockError::BadHighTime { .. })
        ));
        let wide = Clock::new(sig, SimDuration(4));
        assert!(matches!(
            ClockState::new(&wide, 2, "c"),
            Err(ClockError::NotOneBit { .. })
        ));
    }
}
