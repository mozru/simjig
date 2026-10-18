//! Kernel-owned clocks: clocks are data, not tasks (DECISION §1).
//!
//! Each clock is a schedule of edges. At a timestep, every running clock with an edge due
//! writes its new level into the write log (writer: the clock) in declaration order, and
//! all of them share the timestep's first eval. Coincident edges of different clocks
//! therefore land in one eval, which is what clock-domain-crossing designs need.
//!
//! A clock starts low; its first rising edge is at `phase` (default 0), it stays high for
//! `high` ticks (default half the period), and it repeats every `period` ticks.
//!
//! # Control from a running test
//! [`Sim::add_clock`](crate::Sim::add_clock) adds a clock before the run, started at
//! time 0. Inside the run, [`clock`] declares one (stopped) and returns a [`ClockHandle`]
//! that configures, starts and stops it:
//! - `start()` at time `t` behaves like a clock added at `t`: the level due at `t` (high if
//!   `phase` is 0, else low) is written at once, as a write by the clock, so it is applied
//!   after the next eval like any deposit; later edges follow the schedule from `t`.
//!   Started from `main` at time 0 (or from any task woken in a timestep's timed region),
//!   the first edge shares that timestep's first eval with the other clocks' edges.
//! - `stop()` cancels the edges not yet written; the signal keeps its level. A stopped
//!   clock can be reconfigured and started again.
//! - Declaration order, which orders coincident edges, is the order clocks were added or
//!   declared; restarting keeps a clock's place.

use std::fmt;
use std::marker::PhantomData;

use super::OrInvariant;
use super::signal::{self, RawSig, Writer};
use super::state::{self, Kernel, Phase};
use super::time::{SimDuration, SimTime};

/// A clock to be driven by the kernel, added before the run with
/// [`Sim::add_clock`](crate::Sim::add_clock); inside a run, use [`clock`].
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

/// A clock in the kernel's clock table.
#[derive(Debug)]
pub(crate) struct ClockEntry {
    /// The configuration the next `start` uses.
    pub(crate) config: Clock,
    /// The kernel slot the clock drives.
    pub(crate) slot: u32,
    /// The edge schedule, while the clock runs.
    pub(crate) running: Option<ClockState>,
}

impl ClockEntry {
    /// The next edge's time, if the clock is running.
    pub(crate) fn next_edge(&self) -> Option<SimTime> {
        self.running.as_ref().map(|r| r.next_time)
    }
}

/// A validated clock and its next edge.
#[derive(Debug)]
pub(crate) struct ClockState {
    period: u64,
    high: u64,
    pub(crate) next_time: SimTime,
    next_level: u32,
    /// The delayed first rise, pending while the clock sits at its initial low.
    first_rise: Option<u64>,
}

impl ClockState {
    /// Validates `clock` and schedules its first edge at `start`; `width` and `name`
    /// describe its signal.
    pub(crate) fn new(
        clock: &Clock,
        width: u32,
        name: &str,
        start: SimTime,
    ) -> Result<Self, ClockError> {
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
            period,
            high,
            next_time: start,
            next_level: u32::from(phase == 0),
            first_rise: (phase != 0).then(|| start.ticks().saturating_add(phase)),
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

/// A kernel clock, controllable from inside the run; made by [`clock`] and
/// [`Sim::add_clock`](crate::Sim::add_clock).
///
/// `Copy`, valid inside the [`Sim::run`](crate::Sim::run) of its simulation, and `!Send`
/// like the kernel. [`stop`](Self::stop) cancels the edges not yet written and leaves the
/// signal at its level; a stopped clock can be reconfigured and started again. The configuration methods take effect at the next [`start`](Self::start);
/// calling them on a running clock is a mistake that panics, since the change would
/// otherwise be silently deferred.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ClockHandle {
    sim_gen: u32,
    index: usize,
    _single_thread: PhantomData<*const ()>,
}

impl fmt::Debug for ClockHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClockHandle")
            .field("sim", &self.sim_gen)
            .field("index", &self.index)
            .finish()
    }
}

/// Declares a stopped clock of `period` on the 1-bit signal `sig`, or reconfigures the
/// signal's stopped clock (to `period`, phase 0 and a 50 % duty cycle); start it with
/// [`ClockHandle::start`].
///
/// ```
/// # use simjig::{FakeModel, Precision, Sim, clock, ns, rising, now};
/// # let mut model = FakeModel::new(Precision::PS);
/// # model.signal("clk", 1);
/// # let mut sim = Sim::from_fake(model);
/// # let clk = sim.signal("clk").unwrap();
/// let t = sim.run(async move {
///     clock(clk, ns(10)).start();
///     rising(clk).await; // the edge at 0 is applied after the first eval
///     rising(clk).await;
///     now()
/// });
/// assert_eq!(t.unwrap().ticks(), 10_000);
/// ```
///
/// # Panics
/// Outside the signal's running simulation, if the signal is not 1 bit wide, or if it
/// already has a running clock.
#[track_caller]
pub fn clock(sig: impl Into<RawSig>, period: SimDuration) -> ClockHandle {
    let sig = sig.into();
    // Panics are raised here, not in the closure, so they point at the caller.
    let declared = sig.with(|k, slot| {
        let width = k.widths[slot as usize];
        let name = &k.names[slot as usize];
        if width != 1 {
            let signal = name.to_string();
            return Err(ClockError::NotOneBit { signal, width }.to_string());
        }
        let mut clocks = k.clocks.borrow_mut();
        let config = Clock::new(sig, period);
        let index = match clocks.iter().position(|c| c.slot == slot) {
            Some(i) if clocks[i].running.is_some() => {
                return Err(format!(
                    "`{name}` already has a running clock; stop it before declaring another"
                ));
            }
            Some(i) => {
                clocks[i].config = config;
                i
            }
            None => {
                clocks.push(ClockEntry {
                    config,
                    slot,
                    running: None,
                });
                clocks.len() - 1
            }
        };
        Ok(ClockHandle::new(k.sim_gen, index))
    });
    match declared {
        Ok(handle) => handle,
        Err(message) => panic!("{message}"),
    }
}

/// A misuse of a [`ClockHandle`], reported by a panic at the caller.
type Misuse = String;

#[expect(
    clippy::return_self_not_must_use,
    reason = "the methods act on the kernel; returning the handle only allows chaining, so \
              `clock(clk, ns(10)).start();` must not warn"
)]
impl ClockHandle {
    pub(crate) const fn new(sim_gen: u32, index: usize) -> Self {
        Self {
            sim_gen,
            index,
            _single_thread: PhantomData,
        }
    }

    /// Runs `f` on the clock's table entry; panics (at the caller) with `f`'s misuse or
    /// when used outside its simulation.
    #[track_caller]
    fn with<R>(self, f: impl FnOnce(&Kernel, &mut ClockEntry) -> Result<R, Misuse>) -> R {
        let done = state::with_sim(self.sim_gen, |k| {
            let mut clocks = k.clocks.borrow_mut();
            let entry = clocks
                .get_mut(self.index)
                .or_invariant("invariant: a clock handle names a table entry");
            f(k, entry)
        });
        match done {
            Some(Ok(r)) => r,
            Some(Err(message)) => panic!("{message}"),
            None => panic!("a clock handle was used outside the running simulation that made it"),
        }
    }

    /// Configures a stopped clock.
    #[track_caller]
    fn configure(self, what: &str, f: impl FnOnce(&mut Clock)) -> Self {
        self.with(|k, entry| {
            if entry.running.is_some() {
                let name = &k.names[entry.slot as usize];
                return Err(format!(
                    "{what} on the running clock of `{name}`; stop() it first"
                ));
            }
            f(&mut entry.config);
            Ok(())
        });
        self
    }

    /// Delays the first rising edge to `phase` after [`start`](Self::start); the clock
    /// drives low until then.
    ///
    /// # Panics
    /// Outside its simulation, or if the clock is running.
    #[track_caller]
    pub fn phase(self, phase: SimDuration) -> Self {
        self.configure("phase()", |c| *c = c.first_rise_at(phase))
    }

    /// Sets how long the clock stays high in each period (the duty cycle).
    ///
    /// # Panics
    /// Outside its simulation, or if the clock is running.
    #[track_caller]
    pub fn high_for(self, high: SimDuration) -> Self {
        self.configure("high_for()", |c| *c = c.high_for(high))
    }

    /// Starts the clock now, as if it had been added at this time.
    ///
    /// The level due now (high if the phase is 0, else low) is written at once, as a
    /// write by the clock, so it is applied after the next eval like any deposit; later
    /// edges follow the schedule from now. Started from `main` at time 0, or from any task
    /// woken by a `Timer` or `NextTimeStep`, the first edge shares the timestep's first
    /// eval with the other clocks' edges. A restarted clock keeps its place in the order
    /// coincident edges are applied (the order clocks were added or declared).
    ///
    /// # Panics
    /// Outside its simulation, if the clock is already running, in the `ReadOnly` phase
    /// (where its first write would be a failure), or if the period and high time are
    /// invalid (see [`ClockError`]).
    #[track_caller]
    pub fn start(self) -> Self {
        let (slot, level) = self.with(|k, entry| {
            let name = &k.names[entry.slot as usize];
            if entry.running.is_some() {
                return Err(format!("the clock of `{name}` is already running"));
            }
            if k.phase.get() == Phase::ReadOnly {
                return Err(format!(
                    "the clock of `{name}` started in the ReadOnly phase, where its first \
                     edge cannot be written; await NextTimeStep or a Timer first"
                ));
            }
            let width = k.widths[entry.slot as usize];
            let mut schedule = ClockState::new(&entry.config, width, name, k.now.get())
                .map_err(|e| e.to_string())?;
            let level = schedule.take_edge();
            entry.running = Some(schedule);
            Ok((entry.slot, level))
        });
        // The table borrow is released: logging may record a failure, never re-enter it.
        state::with_sim(self.sim_gen, |k| {
            signal::write(k, slot, &[level], Writer::Clock, None);
        });
        self
    }

    /// Stops the clock: edges not yet written are cancelled and the signal keeps its
    /// level. Stopping a stopped clock does nothing.
    ///
    /// # Panics
    /// Outside its simulation.
    #[track_caller]
    pub fn stop(self) {
        self.with(|_, entry| {
            entry.running = None;
            Ok(())
        });
    }

    /// Whether the clock is running.
    ///
    /// # Panics
    /// Outside its simulation.
    #[track_caller]
    #[must_use]
    pub fn is_running(self) -> bool {
        self.with(|_, entry| Ok(entry.running.is_some()))
    }

    /// The signal the clock drives.
    ///
    /// # Panics
    /// Outside its simulation.
    #[track_caller]
    #[must_use]
    pub fn signal(self) -> RawSig {
        self.with(|_, entry| Ok(entry.config.sig()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(clock: &Clock, n: usize) -> Vec<(u64, u32)> {
        let mut state = ClockState::new(clock, 1, "clk", SimTime::ZERO).unwrap();
        (0..n)
            .map(|_| {
                let t = state.next_time.ticks();
                (t, state.take_edge())
            })
            .collect()
    }

    #[test]
    fn edge_schedules_follow_period_high_and_phase() {
        let sig = RawSig::in_model(0, 0, 0);
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
        let sig = RawSig::in_model(0, 0, 0);
        let odd = Clock::new(sig, SimDuration(7));
        assert!(matches!(
            ClockState::new(&odd, 1, "c", SimTime::ZERO),
            Err(ClockError::OddPeriod { .. })
        ));
        let flat = Clock::new(sig, SimDuration(4)).high_for(SimDuration(4));
        assert!(matches!(
            ClockState::new(&flat, 1, "c", SimTime::ZERO),
            Err(ClockError::BadHighTime { .. })
        ));
        let wide = Clock::new(sig, SimDuration(4));
        assert!(matches!(
            ClockState::new(&wide, 2, "c", SimTime::ZERO),
            Err(ClockError::NotOneBit { .. })
        ));
    }
}
