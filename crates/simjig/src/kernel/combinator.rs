//! [`join!`](crate::join) (SV `fork … join`, cocotb `Combine`/`gather`),
//! [`first!`](crate::first) (`fork … join_any; disable fork`, cocotb `First`/`select`) and
//! [`with_timeout`](crate::with_timeout): one mechanism, [`Fork`], modelled on cocotb 2.1's
//! `_wait` (`_concurrent_waiters.py`).
//!
//! # cocotb's mechanism, reproduced
//! - Each operand runs in its own **helper task**. The helpers start at the *front* of the
//!   ready queue, in operand order (`Task._start_next`), so they register before tasks
//!   that were queued earlier but have not run.
//! - The caller waits until **every** helper has finished (cocotb's internal `complete`
//!   event); the last one to finish wakes it at the back of the queue.
//! - `first!` and `with_timeout` (`FIRST_COMPLETED`): the first helper to *finish in run
//!   order* decides, which is not necessarily the leftmost operand. It cancels the others
//!   (`Task.cancel()`): a helper waiting on a trigger is deregistered at once and queued at
//!   the back; one already queued finishes cancelled where it stands. A cancelled helper
//!   whose operand awaits a nested combinator first cancels that combinator's helpers,
//!   then requeues itself until they have finished (`await NullTrigger()` in `_wait`).
//! - `join!` (`FIRST_EXCEPTION`): waits for all; a helper that finishes cancelled (only
//!   when the caller is itself a cancelled helper) cancels the rest.
//!
//! # How a helper runs without owning a future
//! The operands borrow from the caller, so they stay inside the combinator future, which
//! lives in its root task's future. A helper is a kernel entry (`tasks`, `Helper`) with a
//! place in the ready queue and a waker of its own. Running it polls the root task's
//! future *routed* to it (`Kernel::route`): every combinator on the way polls only the
//! operand that contains the target helper, as that helper (`Kernel::context`, and the
//! helper's waker in the operand's `Context`); every other future is merely polled again,
//! with its own task's waker, which futures tolerate. A combinator starts, and returns
//! its result, only in a run of the context that awaits it.
//!
//! Cancellation is still dropping: a cancelled operand's future is dropped when cocotb
//! would deregister it, and its `Drop` cleanup runs then, not when the helper next runs
//! (cocotb's `finally` blocks run then). Dropping the combinator drops every operand and
//! removes its helpers at once.
//!
//! Each operand is boxed, so the macros need no `unsafe` pin projection: one allocation per
//! operand per use, plus one helper entry (no allocation) each.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::OrInvariant;
use super::slab::Key;
use super::state::{self, Kernel};
use super::tasks::{self, HelperState};

/// One operand: the future until it completes, then its output.
#[doc(hidden)]
#[derive(Debug)]
pub enum MaybeDone<F: Future> {
    /// Still running.
    Pending(Pin<Box<F>>),
    /// Completed; the output until `take`n.
    Done(Option<F::Output>),
    /// Cancelled: the future was dropped.
    Cancelled,
}

impl<F: Future> MaybeDone<F> {
    /// Wraps a future.
    pub fn new(future: F) -> Self {
        Self::Pending(Box::pin(future))
    }

    /// The output.
    ///
    /// # Panics
    /// If not done, or taken twice.
    pub fn take(&mut self) -> F::Output {
        match self {
            Self::Done(v) => v
                .take()
                .or_invariant("invariant: a combinator takes each output once"),
            Self::Pending(_) | Self::Cancelled => {
                panic!("invariant: a combinator takes only outputs of operands that completed")
            }
        }
    }
}

/// An operand as [`Fork`] drives it, whatever its output type.
#[doc(hidden)]
pub trait Operand {
    /// Polls if still pending; returns whether it completed now. The future is dropped as
    /// soon as it completes, so its triggers deregister at once.
    fn poll_operand(&mut self, cx: &mut Context<'_>) -> bool;
    /// Drops the future (cancellation).
    fn cancel(&mut self);
}

impl<F: Future> Operand for MaybeDone<F> {
    fn poll_operand(&mut self, cx: &mut Context<'_>) -> bool {
        let Self::Pending(future) = self else {
            return false;
        };
        match future.as_mut().poll(cx) {
            Poll::Ready(v) => {
                *self = Self::Done(Some(v));
                true
            }
            Poll::Pending => false,
        }
    }

    fn cancel(&mut self) {
        if matches!(self, Self::Pending(_)) {
            *self = Self::Cancelled;
        }
    }
}

/// When a combinator's caller is decided (cocotb's `return_when`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// `FIRST_COMPLETED`: the first helper to finish decides and cancels the rest.
    First,
    /// `FIRST_EXCEPTION`: all must finish; one that finishes cancelled cancels the rest.
    All,
}

/// How a helper finished.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Completed,
    Cancelled,
}

/// The helper tasks of one combinator of `N` operands; see the module docs.
#[doc(hidden)]
#[derive(Debug)]
pub struct Fork<const N: usize> {
    wait: Wait,
    run: Option<Run<N>>,
}

#[derive(Debug)]
struct Run<const N: usize> {
    sim_gen: u32,
    /// The context awaiting the combinator: its task, or a helper of an outer combinator.
    owner: Key,
    helpers: [Key; N],
    /// Helpers not yet finished.
    remaining: usize,
    /// The operand that decided (cocotb's `first_completed`).
    decided: Option<usize>,
    /// Whether the remaining helpers were cancelled.
    cancelled: bool,
}

impl<const N: usize> Fork<N> {
    /// For `first!` and `with_timeout`.
    #[must_use]
    pub const fn first() -> Self {
        Self {
            wait: Wait::First,
            run: None,
        }
    }

    /// For `join!`.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            wait: Wait::All,
            run: None,
        }
    }

    /// Drives the combinator: starts the helpers on the first poll, runs the helper this
    /// poll is routed to, and returns once every helper finished, in a run of the caller.
    /// The output is the deciding operand (`first!`), or 0 (`join!`).
    ///
    /// The caller's own waker is not needed: the kernel wakes the caller when the last
    /// helper finishes.
    pub fn poll(&mut self, ops: [&mut dyn Operand; N], _cx: &mut Context<'_>) -> Poll<usize> {
        // One kernel lookup per poll; operands polled inside reach the kernel the same way.
        state::with_current("a combinator", |k| self.poll_in(k, ops))
    }

    fn poll_in(&mut self, k: &Kernel, mut ops: [&mut dyn Operand; N]) -> Poll<usize> {
        let wait = self.wait;
        let Some(run) = &mut self.run else {
            let (owner, helpers) = tasks::start_helpers::<N>(k);
            self.run = Some(Run {
                sim_gen: k.sim_gen,
                owner,
                helpers,
                remaining: N,
                decided: None,
                cancelled: false,
            });
            return Poll::Pending;
        };
        if tasks::runs_owner(k) {
            if run.remaining == 0 {
                let decided = run.decided.unwrap_or(0);
                self.free();
                return Poll::Ready(decided);
            }
            // The caller is a helper being cancelled: cancel this combinator's helpers
            // (`_wait`'s `except CancelledError: cancel_remaining()`).
            if !run.cancelled && tasks::owner_cancelling(k, run.owner) {
                run.cancel_remaining(k, &mut ops);
            }
            return Poll::Pending;
        }
        if let Some(i) = tasks::route_index(k, &run.helpers) {
            run.step(k, wait, i, &mut ops);
        }
        Poll::Pending
    }

    fn free(&mut self) {
        if let Some(run) = self.run.take() {
            state::with_sim(run.sim_gen, |k| {
                tasks::free_helpers(k, &run.helpers, run.owner, run.remaining > 0);
            });
        }
    }
}

impl<const N: usize> Drop for Fork<N> {
    fn drop(&mut self) {
        self.free();
    }
}

impl<const N: usize> Run<N> {
    /// Runs helper `i`, or passes this run through it to a combinator in its operand.
    fn step(&mut self, k: &Kernel, wait: Wait, i: usize, ops: &mut [&mut dyn Operand; N]) {
        let h = self.helpers[i];
        let (state, own_run) = (tasks::helper_state(k, h), tasks::routed_to(k, h));
        match state {
            HelperState::Done => {}
            HelperState::Cancelling if own_run => {
                // The first run after `cancel()` passes it on to nested combinators, then
                // waits (requeued) until their helpers finished.
                if tasks::take_propagation(k, h) {
                    poll_as(k, h, &mut *ops[i]);
                }
                if tasks::has_live_forks(k, h) {
                    tasks::requeue(k, h);
                } else {
                    cancel_operand(k, &mut *ops[i]);
                    self.finished(k, wait, i, Outcome::Cancelled, ops);
                }
            }
            HelperState::Running | HelperState::Cancelling => {
                if poll_as(k, h, &mut *ops[i]) {
                    self.finished(k, wait, i, Outcome::Completed, ops);
                }
            }
        }
    }

    /// cocotb's `done_callback`: helper `i` finished.
    fn finished(
        &mut self,
        k: &Kernel,
        wait: Wait,
        i: usize,
        outcome: Outcome,
        ops: &mut [&mut dyn Operand; N],
    ) {
        tasks::finish_helper(k, self.helpers[i]);
        self.remaining -= 1;
        if self.remaining == 0 {
            tasks::fork_complete(k, self.owner);
        }
        let decides = wait == Wait::First || outcome == Outcome::Cancelled;
        if decides && self.decided.is_none() {
            self.decided = Some(i);
            self.cancel_remaining(k, ops);
        }
    }

    /// cocotb's `cancel_remaining`: cancels every helper not finished, in operand order.
    fn cancel_remaining(&mut self, k: &Kernel, ops: &mut [&mut dyn Operand; N]) {
        if self.cancelled {
            return;
        }
        self.cancelled = true;
        for (op, &h) in ops.iter_mut().zip(&self.helpers) {
            if tasks::helper_state(k, h) == HelperState::Running && tasks::cancel_helper(k, h) {
                cancel_operand(k, &mut **op);
            }
        }
    }
}

/// Cancels `op`, marking the drop as the combinator's: a `Task` passed by value that lost
/// is cancelled by design (`docs/guide.md`, "Differences from cocotb"), not dropped by
/// mistake, even if it never ran.
fn cancel_operand(k: &Kernel, op: &mut dyn Operand) {
    struct Restore<'k>(&'k Kernel, bool);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            self.0.cancelling_operand.set(self.1);
        }
    }
    let _restore = Restore(k, k.cancelling_operand.replace(true));
    op.cancel();
}

/// Polls `op` as helper `h`; returns whether it completed.
fn poll_as(k: &Kernel, h: Key, op: &mut dyn Operand) -> bool {
    let (waker, previous) = tasks::enter_helper(k, h);
    let done = op.poll_operand(&mut Context::from_waker(&waker));
    k.context.set(previous);
    done
}

/// Awaits several futures concurrently and returns all their outputs as a tuple, like SV
/// `fork … join` and cocotb's `Combine`/`gather`. Must be used inside an `async` context.
///
/// Each operand runs as a helper task, and the caller resumes when the last one finishes,
/// after the tasks already ready then (see the module docs of `kernel::combinator`).
///
/// ```
/// # use simjig::{FakeModel, Precision, Sim, SimDuration, join, timer};
/// let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
/// let out = sim.run(async {
///     join!(
///         async { timer(SimDuration::from_ticks(2)).await; 'a' },
///         async { 7 },
///     )
/// });
/// assert_eq!(out.unwrap(), ('a', 7));
/// ```
#[macro_export]
macro_rules! join {
    ($($fut:expr),+ $(,)?) => {
        $crate::__fork!(@acc all [] $($fut,)+)
    };
}

/// Awaits several futures with the same output type and returns the output of the first
/// to complete, cancelling (dropping) the others, like SV `fork … join_any; disable fork`
/// and cocotb's `First`/`select`. Must be used inside an `async` context.
///
/// Each operand runs as a helper task; the first helper to *finish* wins. Helpers start in
/// operand order, so of operands woken by different triggers in one timestep the one whose
/// trigger fires first wins, and of operands woken together the leftmost; an operand woken
/// by a task the same trigger woke first loses to one woken by the trigger itself. The
/// caller resumes once the cancelled losers have finished too (see the module docs of
/// `kernel::combinator`).
///
/// ```
/// # use simjig::{FakeModel, Precision, Sim, SimDuration, first, timer};
/// let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
/// let out = sim.run(async {
///     first!(
///         async { timer(SimDuration::from_ticks(5)).await; "slow" },
///         async { timer(SimDuration::from_ticks(2)).await; "fast" },
///     )
/// });
/// assert_eq!(out.unwrap(), "fast");
/// ```
///
/// To know *which* operand won when their outputs differ (cocotb's
/// `First(RisingEdge(done), Timer(..))` then `if t is timer`), end each operand with a
/// variant of a small enum of your own and `match` on the result:
///
/// ```
/// # use simjig::{FakeModel, Precision, Sim, SimDuration, first, timer};
/// enum Won {
///     Done(u32),
///     TimedOut,
/// }
/// let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
/// let out = sim.run(async {
///     match first!(
///         async { timer(SimDuration::from_ticks(2)).await; Won::Done(7) },
///         async { timer(SimDuration::from_ticks(500)).await; Won::TimedOut },
///     ) {
///         Won::Done(n) => n,
///         Won::TimedOut => panic!("done within 500 ns"),
///     }
/// });
/// assert_eq!(out.unwrap(), 7);
/// ```
///
/// For one operand and a timeout, [`with_timeout`](crate::with_timeout) returns a
/// `Result` instead.
#[macro_export]
macro_rules! first {
    ($($fut:expr),+ $(,)?) => {
        $crate::__fork!(@acc first [] $($fut,)+)
    };
}

/// The expansion shared by [`join!`] and [`first!`]: operands in a tuple of
/// `MaybeDone`s. Each recursion step names its operand `op`; macro hygiene makes the
/// names distinct, so one pattern binds them all.
#[doc(hidden)]
#[macro_export]
macro_rules! __fork {
    (@acc $wait:ident [$($done:tt)*] $head:expr, $($rest:expr,)*) => {
        $crate::__fork!(@acc $wait [$($done)* (op $head)] $($rest,)*)
    };
    (@acc $wait:ident [$( ($op:ident $fut:expr) )*]) => {{
        let mut operands = ( $( $crate::__private::MaybeDone::new($fut), )* );
        let mut fork = $crate::__private::Fork::$wait();
        let decided = ::core::future::poll_fn(|cx| {
            let ( $( $op, )* ) = &mut operands;
            fork.poll([ $( $op as &mut dyn $crate::__private::Operand, )* ], cx)
        })
        .await;
        ::core::mem::drop(fork);
        let ( $( $op, )* ) = &mut operands;
        $crate::__fork!(@out $wait decided [$( $op )*])
    }};
    (@out all $decided:ident [$( $op:ident )*]) => {{
        let _ = $decided;
        ( $( $op.take(), )* )
    }};
    (@out first $decided:ident [$( $op:ident )*]) => {{
        let mut index = 0_usize;
        let mut out = ::core::option::Option::None;
        $(
            if index == $decided {
                out = ::core::option::Option::Some($op.take());
            }
            index += 1;
        )*
        let _ = index;
        match out {
            ::core::option::Option::Some(v) => v,
            ::core::option::Option::None => {
                ::core::unreachable!("invariant: first! returns the operand that decided")
            }
        }
    }};
}
