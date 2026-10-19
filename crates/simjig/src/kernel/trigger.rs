//! Triggers: futures that resume a task at a point of the scheduler loop (cocotb 2.x
//! semantics on Verilator).
//!
//! | Trigger | Resumes |
//! |---|---|
//! | [`rising`], [`falling`], [`changed`] | when the comparison after a write-log application sees that change |
//! | [`cycles`] | after `n` rising edges |
//! | [`Timer`] | in the timed region of timestep `now + d`, *before* its clock edges |
//! | [`NextTimeStep`] | at the start of the next timestep, before its `Timer`s |
//! | [`ReadWrite`] | in the `ReadWrite` region of this timestep, once the deltas settled |
//! | [`ReadOnly`] | in the `ReadOnly` region of this timestep; writes are failures there |
//! | [`yield_now`] | after the other ready tasks |
//!
//! [`TriggerExt`] adds `.read_write()` and `.read_only()` to the edge, cycle and timer
//! triggers: the trigger, then that region of the same timestep, in one expression.
//!
//! Edge timing follows from the loop (see `sched`): an edge of a signal the testbench
//! drives (a kernel clock, an input) fires *before* the eval that reacts to it, so right
//! after `rising(clk)` flops still show their old values (cocotb on Verilator does the
//! same); an edge of a design-driven signal fires after the eval that made it, with
//! post-eval values. A write is never captured by the edge that woke the writer.
//!
//! The signal triggers take a [`RawSig`] or a writable typed handle (`In`, `InOut`,
//! `PublicRw`), like [`clock`](crate::clock); every typed handle, outputs included, also
//! has them as methods (`dut.ready.rising()`).
//!
//! All of them register on first poll and deregister in `Drop`, so dropping one (for
//! example as the loser of [`first!`](crate::first)) leaves nothing behind. Edges are seen
//! only at eval boundaries: a pulse that starts and ends inside one eval is invisible.

use std::fmt;
use std::future::Future;
use std::panic::Location;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::combinator::{Fork, MaybeDone};
use super::failure::{Failure, FailureKind};
use super::panics;
use super::signal::RawSig;
use super::state::{self, Kernel, Phase};
use super::tasks;
use super::time::{Precision, SimDuration, SimTime, TimeDisplay};
use super::waits::{EdgeKind, Registration, WaitOn};

/// Shared poll logic: register on first poll, then complete once fired.
///
/// `at` is where the trigger was made: misuse that `register` detects (it returns the
/// message) panics there, at the user's line, rather than in this module.
fn poll_registered(
    reg: &mut Option<Registration>,
    cx: &Context<'_>,
    what: &str,
    at: &'static Location<'static>,
    register: impl FnOnce(&Kernel) -> Result<WaitOn, String>,
) -> Poll<()> {
    match reg {
        None => {
            let registered = state::try_current(what, |k| {
                if k.phase.get() == Phase::Sync {
                    return Err(format!(
                        "{what} awaited in a synchronous test (`#[simjig::test(sync)]`): it has \
                         no scheduler, tasks or triggers; step the clock with `SyncClock` (or make \
                         the test async)"
                    ));
                }
                register(k).map(|on| Registration::new(k, on, cx, Some(at)))
            });
            match registered {
                Some(Ok(r)) => *reg = Some(r),
                Some(Err(message)) => panics::panic_at(at, format_args!("{message}")),
                None => panics::panic_at(
                    at,
                    format_args!("{what} needs a running simulation; await it inside Sim::run"),
                ),
            }
            Poll::Pending
        }
        Some(r) => {
            if r.take_fired(cx) {
                *reg = None;
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}

/// Waits for a value change of a signal; made by [`rising`], [`falling`] and [`changed`].
#[derive(Debug)]
#[must_use = "triggers do nothing unless awaited"]
pub struct Edge {
    slot: u32,
    kind: EdgeKind,
    at: &'static Location<'static>,
    reg: Option<Registration>,
}

#[track_caller]
fn edge(sig: RawSig, edge: EdgeKind, what: &str) -> Edge {
    let slot = sig.with(|k, slot| {
        let width = k.widths[slot as usize];
        assert!(
            edge == EdgeKind::Changed || width == 1,
            "{what}(`{}`): edges are defined on 1-bit signals and this one is {width} bits; \
             use changed()",
            k.names[slot as usize]
        );
        slot
    });
    Edge {
        slot,
        kind: edge,
        at: Location::caller(),
        reg: None,
    }
}

/// Resumes after an eval in which the 1-bit signal went from 0 to 1.
///
/// # Panics
/// Outside the signal's running simulation, or if it is not 1 bit wide.
#[track_caller]
pub fn rising(sig: impl Into<RawSig>) -> Edge {
    edge(sig.into(), EdgeKind::Rising, "rising")
}

/// Resumes after an eval in which the 1-bit signal went from 1 to 0.
///
/// # Panics
/// As [`rising`].
#[track_caller]
pub fn falling(sig: impl Into<RawSig>) -> Edge {
    edge(sig.into(), EdgeKind::Falling, "falling")
}

/// Resumes after an eval that changed any bit of the signal.
///
/// # Panics
/// Outside the signal's running simulation.
#[track_caller]
pub fn changed(sig: impl Into<RawSig>) -> Edge {
    edge(sig.into(), EdgeKind::Changed, "changed")
}

impl Future for Edge {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let (slot, edge, at) = (self.slot, self.kind, self.at);
        poll_registered(&mut self.reg, cx, "an edge trigger", at, |_| {
            Ok(WaitOn::Edge { slot, edge })
        })
    }
}

/// Waits for `n` rising edges of `sig`; `n == 0` returns at once.
///
/// `n` is any integer (`u32`, `usize`, `u64`, ...): `TryInto<u64>` rather than
/// `Into<u64>`, because `usize` has no `Into<u64>`, and because an unsuffixed literal
/// (`cycles(clk, 5)`) falls back to `i32`, which has none either.
///
/// # Panics
/// Here, if `n` is negative (or does not fit in a `u64`); otherwise as [`rising`].
#[track_caller]
pub fn cycles(sig: impl Into<RawSig>, n: impl TryInto<u64>) -> Cycles {
    let sig = sig.into();
    let Ok(n) = n.try_into() else {
        panic!("cycles: the count of edges must be a non-negative integer that fits in u64")
    };
    Cycles {
        sig,
        left: n,
        edge: None,
    }
}

/// Waits for a number of rising edges; made by [`cycles`].
///
/// A named type rather than an `async` block so that it can take
/// [`TriggerExt::read_write`] and [`TriggerExt::read_only`].
#[derive(Debug)]
#[must_use = "triggers do nothing unless awaited"]
pub struct Cycles {
    sig: RawSig,
    left: u64,
    edge: Option<Edge>,
}

impl Future for Cycles {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self;
        while this.left > 0 {
            let sig = this.sig;
            let edge = this.edge.get_or_insert_with(|| rising(sig));
            if Pin::new(edge).poll(cx).is_pending() {
                return Poll::Pending;
            }
            this.edge = None;
            this.left -= 1;
        }
        Poll::Ready(())
    }
}

/// Resumes `d` ticks after it is first awaited, in the timed region of that timestep:
/// before that timestep's clock edges are applied, so it sees pre-edge values and wins a
/// tie against an edge in [`first!`](crate::first) or [`with_timeout`].
#[derive(Debug)]
#[must_use = "triggers do nothing unless awaited"]
pub struct Timer {
    duration: SimDuration,
    at: &'static Location<'static>,
    reg: Option<Registration>,
}

impl Timer {
    /// A timer for `duration`, counted from its first poll.
    ///
    /// # Panics
    /// When awaited, if `duration` is zero ticks or the deadline overflows the tick
    /// counter; the panic points at this call.
    ///
    /// cocotb 2.1 rejects `Timer(0)` when it is built (`ValueError`). This constructor
    /// stays `const` (usable in `const` items) and only records the call site; the check
    /// runs on the first poll, inside the run, so it fails the run as a `TaskPanic` whose
    /// message names this call, even for a `Timer` built before
    /// [`Sim::run`](crate::Sim::run). A zero `Timer` that is never awaited is inert.
    #[track_caller]
    pub const fn new(duration: SimDuration) -> Self {
        Self {
            duration,
            at: Location::caller(),
            reg: None,
        }
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let (duration, at) = (self.duration, self.at);
        poll_registered(&mut self.reg, cx, "Timer", at, |k| {
            if duration.ticks() == 0 {
                return Err(
                    "Timer of zero ticks; use yield_now() or ReadWrite to stay in this \
                            timestep"
                        .to_owned(),
                );
            }
            let deadline = k
                .now
                .get()
                .checked_add(duration)
                .ok_or_else(|| "Timer deadline overflows the 64-bit tick counter".to_owned())?;
            Ok(WaitOn::Time(deadline))
        })
    }
}

macro_rules! phase_trigger {
    ($(#[$doc:meta])* $name:ident, $on:expr, $check:expr) => {
        $(#[$doc])*
        #[derive(Debug)]
        #[must_use = "triggers do nothing unless awaited"]
        pub struct $name {
            at: &'static Location<'static>,
            reg: Option<Registration>,
        }

        impl $name {
            /// The trigger, registered when first awaited; misuse panics point at this call.
            #[track_caller]
            pub const fn new() -> Self {
                Self {
                    at: Location::caller(),
                    reg: None,
                }
            }
        }

        impl Default for $name {
            #[track_caller]
            fn default() -> Self {
                Self::new()
            }
        }

        impl Future for $name {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                let at = self.at;
                poll_registered(&mut self.reg, cx, stringify!($name), at, |k| {
                    let check: fn(&Kernel) -> Result<(), String> = $check;
                    check(k)?;
                    Ok($on)
                })
            }
        }
    };
}

fn not_in_read_only(k: &Kernel, what: &str) -> Result<(), String> {
    if k.phase.get() == Phase::ReadOnly {
        return Err(format!(
            "{what} awaited in the ReadOnly phase, which is already past it; await \
             NextTimeStep, a Timer or an edge first"
        ));
    }
    Ok(())
}

phase_trigger!(
    /// Resumes in the `ReadWrite` region of the current timestep, after the delta loop
    /// settled: writes logged before awaiting are applied and their fan-out evaluated.
    /// Awaited from inside the `ReadWrite` region, it resumes in the next one.
    ///
    /// # Panics
    /// When awaited in the `ReadOnly` phase.
    ReadWrite,
    WaitOn::ReadWrite,
    |k| not_in_read_only(k, "ReadWrite")
);

phase_trigger!(
    /// Resumes in the `ReadOnly` phase of the current timestep, once no more writes are
    /// pending; values are final for this timestep and any write is a failure.
    ///
    /// # Panics
    /// When awaited in the `ReadOnly` phase.
    ReadOnly,
    WaitOn::ReadOnly,
    |k| not_in_read_only(k, "ReadOnly")
);

phase_trigger!(
    /// Resumes at the start of the next timestep, whenever something else schedules one.
    NextTimeStep,
    WaitOn::NextTimeStep,
    |_| Ok(())
);

mod sealed {
    /// Only the edge, cycle and timer triggers take a region suffix.
    pub trait Sealed {}
    impl Sealed for super::Edge {}
    impl Sealed for super::Cycles {}
    impl Sealed for super::Timer {}
}

/// `.read_write()` and `.read_only()` on [`rising`], [`falling`], [`changed`] (functions
/// and handle methods), [`cycles`] and [`Timer`]: await the trigger, then the
/// [`ReadWrite`] (respectively [`ReadOnly`]) region of the same timestep.
///
/// `dut.clk.rising().read_write().await` is `dut.clk.rising().await;
/// ReadWrite::new().await;` in one expression: flops clocked by that edge show their new
/// values and writes are still allowed, though a later write in the timestep may change
/// combinational values again. After `.read_only()` values are final for the timestep and
/// any write fails the run (`WriteInReadOnly`). cocotb spells these `await RisingEdge(clk);
/// await ReadWrite()` and `await ReadOnly()`.
///
/// Sealed: [`Event`](crate::Event), [`Lock`](crate::Lock) and [`Queue`](crate::Queue) can
/// resume a task in any region, where "the region of the same timestep" may already be
/// past.
pub trait TriggerExt: sealed::Sealed + Future<Output = ()> + Unpin + Sized {
    /// Awaits this trigger, then the [`ReadWrite`] region of the timestep it fired in.
    ///
    /// # Panics
    /// When awaited, if this trigger completes in the `ReadOnly` phase (only `cycles(.., 0)`
    /// can); the panic points at this call.
    #[track_caller]
    fn read_write(self) -> ThenRegion<Self, ReadWrite> {
        ThenRegion {
            trigger: Some(self),
            region: ReadWrite::new(),
        }
    }

    /// Awaits this trigger, then the [`ReadOnly`] region of the timestep it fired in.
    ///
    /// # Panics
    /// As [`read_write`](Self::read_write).
    #[track_caller]
    fn read_only(self) -> ThenRegion<Self, ReadOnly> {
        ThenRegion {
            trigger: Some(self),
            region: ReadOnly::new(),
        }
    }
}

impl<T: sealed::Sealed + Future<Output = ()> + Unpin> TriggerExt for T {}

/// A trigger followed by a region of the same timestep; made by [`TriggerExt`].
///
/// Each half registers when first polled and deregisters when dropped, so dropping this
/// (as the loser of [`first!`](crate::first)) leaves nothing registered.
#[derive(Debug)]
#[must_use = "triggers do nothing unless awaited"]
pub struct ThenRegion<T, R> {
    trigger: Option<T>,
    region: R,
}

impl<T, R> Future for ThenRegion<T, R>
where
    T: Future<Output = ()> + Unpin,
    R: Future<Output = ()> + Unpin,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self;
        if let Some(trigger) = &mut this.trigger {
            if Pin::new(trigger).poll(cx).is_pending() {
                return Poll::Pending;
            }
            this.trigger = None;
        }
        // The region trigger registers now, in the timestep the trigger fired in.
        Pin::new(&mut this.region).poll(cx)
    }
}

/// Returned by [`yield_now`].
#[derive(Debug, Default)]
#[must_use = "triggers do nothing unless awaited"]
pub struct YieldNow {
    yielded: bool,
}

/// Lets the other ready tasks of the current delta run first.
pub const fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

impl Future for YieldNow {
    type Output = ();

    // One hop (cocotb's `NullTrigger`, or any trigger that fires as it is primed): the
    // first poll queues the task at the back; the hop ends in the next run of the task (or
    // helper) that awaits it. A hand-written combinator in the task's future may poll it
    // again in a run routed to one of the task's combinator helpers (`combinator`); that
    // is not the task's own run and must not end the hop early. Waking again there is a
    // no-op while the task is queued, and requeues it if it already ran.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let own_run = state::try_current("yield_now()", tasks::runs_owner).unwrap_or(true);
        if self.yielded && own_run {
            return Poll::Ready(());
        }
        self.yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// The current simulation time, in ticks of the design's precision. To print it in
/// physical units, use [`now_display`] (`1250 ns`), or [`tb_log!`](crate::tb_log) for a
/// line with the time and the task.
///
/// # Panics
/// Outside [`Sim::run`](crate::Sim::run).
#[must_use]
#[track_caller]
pub fn now() -> SimTime {
    state::with_current("now()", |k| k.now.get())
}

/// The current simulation time in physical units, ready to print (`1250 ns`): `now()`
/// displayed with the running simulation's precision, which no caller has to thread
/// through. [`tb_log!`](crate::tb_log) prints it with the task's name.
///
/// # Panics
/// Outside [`Sim::run`](crate::Sim::run).
#[must_use]
#[track_caller]
pub fn now_display() -> TimeDisplay {
    state::with_current("now_display()", |k| k.now.get().display(k.precision))
}

/// The `[<time> <task>]` prefix of [`tb_log!`](crate::tb_log): the time as [`now_display`]
/// prints it and the hierarchical name of the running task.
#[doc(hidden)]
#[must_use]
#[track_caller]
pub fn log_prefix() -> String {
    state::with_current("simjig::tb_log!", |k| {
        let time = k.now.get().display(k.precision);
        match k.current.get() {
            Some(task) => format!("[{time} {}]", k.task_name(task)),
            None => format!("[{time}]"),
        }
    })
}

/// Prints a line to stdout, prefixed with the simulation time in physical units and the
/// running task's name, like cocotb's `dut._log.info`: `[1250 ns monitor] hdr len=3`.
/// Takes `format!` arguments. Inside a test, the harness captures stdout like any
/// `println!` (shown for a failing test, or with `--nocapture`).
///
/// ```
/// # use simjig::{FakeModel, Precision, Sim, SimDuration, Timer, spawn};
/// let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
/// sim.run(async {
///     spawn("monitor", async {
///         Timer::new(SimDuration::from_ticks(1250)).await;
///         simjig::tb_log!("hdr len={}", 3); // [1250 ns monitor] hdr len=3
///     })
///     .await;
/// })
/// .unwrap();
/// ```
///
/// Named `tb_log!`, not `log!`: the `log` crate exports a `log!` macro too, and a
/// testbench that glob-imports both crates (`use log::*; use simjig::*;`) could not call
/// either by its bare name.
///
/// # Panics
/// Outside [`Sim::run`](crate::Sim::run).
#[macro_export]
macro_rules! tb_log {
    ($($arg:tt)*) => {
        ::std::println!(
            "{} {}",
            $crate::__private::log_prefix(),
            ::core::format_args!($($arg)*)
        )
    };
}

/// [`with_timeout`] ran out of time.
///
/// It converts into a [`Failure`] ([`FailureKind::Timeout`]), so `with_timeout(..).await?`
/// works in a test returning [`simjig::Result`](crate::Result).
///
/// `Debug` (what `.unwrap()` and `.expect(..)` print) shows the times in physical units:
/// `Elapsed { after: 3 ns, at: 5 ns, precision: 1ns }`.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Elapsed {
    /// The timeout that elapsed.
    pub after: SimDuration,
    /// When it elapsed.
    pub at: SimTime,
    /// The design's precision, to print `after` and `at` in physical units.
    pub precision: Precision,
}

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let after = SimTime(self.after.ticks()).display(self.precision);
        write!(f, "timed out after {after}")
    }
}

/// Ticks mean nothing without the precision, and every `.expect()` on a timeout prints
/// this, so the fields print in physical units, as [`SimTime::display`] does.
impl fmt::Debug for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Elapsed")
            .field(
                "after",
                &format_args!("{}", SimTime(self.after.ticks()).display(self.precision)),
            )
            .field("at", &format_args!("{}", self.at.display(self.precision)))
            .field("precision", &format_args!("{}", self.precision))
            .finish()
    }
}

impl std::error::Error for Elapsed {}

impl From<Elapsed> for Failure {
    /// A failure of the task converting it (the one that awaited [`with_timeout`], for `?`),
    /// at the `?` (`#[track_caller]`: `?` passes its location through `From::from`).
    #[track_caller]
    fn from(e: Elapsed) -> Self {
        let task = state::try_current("Failure::from(Elapsed)", |k| {
            k.current.get().map(|key| k.task_name(key))
        })
        .flatten();
        Self {
            kind: FailureKind::Timeout { after: e.after },
            time: e.at,
            precision: e.precision,
            task,
            location: Some(Location::caller().into()),
            message: e.to_string(),
        }
    }
}

/// Runs `future` for at most `duration`; on timeout it is cancelled (dropped).
///
/// This is cocotb 2.1's `with_timeout`, `select(Timer(timeout), trigger)`: a
/// [`first!`](crate::first) of the timeout's Timer and `future`, each in a helper task, the
/// Timer's first. Ties break as in cocotb (golden `timeout_ties`):
/// - the timeout's Timer registers before anything `future` waits on, and Timers due at
///   the same time fire one at a time in registration order, so a `Timer` (inline, or in a
///   task spawned just before) due when the timeout is loses to it;
/// - Timers resume before that timestep's edges, so an edge or `cycles` due when the
///   timeout is loses too;
/// - a spawned task whose own Timer was registered *before* the timeout's finishes first
///   (it runs, with what it wakes, before the timeout's Timer fires), so its result wins.
///
/// The caller resumes where cocotb's `select` resumes it: once the cancelled side has
/// finished too (see `kernel::combinator`, golden `combinator_hop`).
///
/// # Errors
/// [`Elapsed`] if the timeout expired first.
///
/// # Panics
/// When awaited, if `duration` is zero ticks (cocotb 2.1's `Timer(0)` raises
/// `ValueError`); the panic names `with_timeout` and points at this call.
///
/// # Cancellation
/// Cancel-safe exactly when `future` is: a timeout drops it mid-way.
#[track_caller]
pub fn with_timeout<T>(
    duration: SimDuration,
    future: impl Future<Output = T>,
) -> impl Future<Output = Result<T, Elapsed>> {
    // Not an `async fn`: `#[track_caller]` needs a plain fn to capture the user's call.
    let at = Location::caller();
    async move {
        if duration.ticks() == 0 {
            panics::panic_at(
                at,
                format_args!(
                    "with_timeout of zero ticks; pass a positive duration (cocotb rejects \
                     a zero timeout too)"
                ),
            );
        }
        let mut timer = MaybeDone::new(Timer {
            duration,
            at,
            reg: None,
        });
        let mut inner = MaybeDone::new(future);
        let mut fork = Fork::first();
        let decided = std::future::poll_fn(|cx| fork.poll([&mut timer, &mut inner], cx)).await;
        drop(fork);
        if decided == 0 {
            let (at, precision) =
                state::with_current("with_timeout", |k| (k.now.get(), k.precision));
            return Err(Elapsed {
                after: duration,
                at,
                precision,
            });
        }
        Ok(inner.take())
    }
}
