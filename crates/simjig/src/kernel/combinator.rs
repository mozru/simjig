//! [`join!`](crate::join) (SV `fork … join`) and [`first!`](crate::first) (`fork …
//! join_any` followed by `disable fork`).
//!
//! Both poll their operands left to right on every wake, so ties resolve deterministically
//! toward the leftmost operand. Each operand is boxed so the macros need no `unsafe` pin
//! projection; that costs one allocation per operand per use.
//!
//! # When the caller resumes (cocotb 2.1, golden `combinator_hop`)
//! cocotb's `First`, `Combine` and `with_timeout` await each operand in a helper task and
//! wake the caller through an internal event once the helpers are done; a helper that
//! loses a `First` is cancelled by scheduling it once more. So the caller resumes after
//! the tasks the same callback woke directly: one event-loop hop after the deciding
//! operand completes, and a second one when a loser was still waiting and had to be
//! cancelled. simjig polls operands inline in the caller and reproduces the order by
//! yielding the same number of times ([`hop`]) instead of spawning helpers: each yield
//! puts the caller at the back of the ready queue, where cocotb's next hop would run.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::OrInvariant;
use super::trigger::yield_now;
use super::waits::cancelled_waits;

/// One operand of `join!`: the future until it completes, then its output.
#[doc(hidden)]
#[derive(Debug)]
pub enum MaybeDone<F: Future> {
    /// Still running.
    Pending(Pin<Box<F>>),
    /// Completed; the output until `take`n.
    Done(Option<F::Output>),
}

impl<F: Future> MaybeDone<F> {
    /// Wraps a future.
    pub fn new(future: F) -> Self {
        Self::Pending(Box::pin(future))
    }

    /// Polls if still pending; returns whether it is done. The future is dropped as soon
    /// as it completes, so its triggers deregister at once.
    pub fn poll_step(&mut self, cx: &mut Context<'_>) -> bool {
        if let Self::Pending(future) = self {
            match future.as_mut().poll(cx) {
                Poll::Ready(v) => *self = Self::Done(Some(v)),
                Poll::Pending => return false,
            }
        }
        true
    }

    /// The output.
    ///
    /// # Panics
    /// If not done, or taken twice.
    pub fn take(&mut self) -> F::Output {
        match self {
            Self::Done(v) => v
                .take()
                .or_invariant("invariant: join! takes each output once"),
            Self::Pending(_) => panic!("invariant: join! takes outputs only when all are done"),
        }
    }
}

/// The hops of a finished combinator: one yield, and a second if dropping `losers`
/// cancelled a wait that had not fired (cocotb cancels a still-pending loser's helper task
/// by scheduling it once more, and wakes the caller only after that).
#[doc(hidden)]
pub async fn hop<L>(losers: L) {
    let before = cancelled_waits();
    drop(losers);
    hops(cancelled_waits() != before).await;
}

/// One yield, and a second if `pending_loser`.
pub(crate) async fn hops(pending_loser: bool) {
    yield_now().await;
    if pending_loser {
        yield_now().await;
    }
}

/// Boxes a `first!` operand so operands of different future types share one array type.
#[doc(hidden)]
pub fn boxed<'a, T>(future: impl Future<Output = T> + 'a) -> Pin<Box<dyn Future<Output = T> + 'a>> {
    Box::pin(future)
}

/// Awaits several futures concurrently and returns all their outputs as a tuple, like SV
/// `fork … join`. Must be used inside an `async` context.
///
/// Like cocotb's `Combine`, the caller resumes one hop after the last operand completes:
/// after the other tasks ready at that point (see the module docs).
///
/// ```
/// # use simjig::{FakeModel, Precision, Sim, SimDuration, Timer, join};
/// let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
/// let out = sim.run(async {
///     join!(
///         async { Timer::new(SimDuration::from_ticks(2)).await; 'a' },
///         async { 7 },
///     )
/// });
/// assert_eq!(out.unwrap(), ('a', 7));
/// ```
#[macro_export]
macro_rules! join {
    (@acc [$($skip:tt)*] [$($done:tt)*] $head:expr, $($rest:expr,)*) => {
        $crate::join!(@acc [$($skip)* _] [$($done)* (($($skip)*) $head)] $($rest,)*)
    };
    (@acc [$($skip:tt)*] [$( (($($s:tt)*) $fut:expr) )*]) => {{
        let mut futures = ( $( $crate::__private::MaybeDone::new($fut), )* );
        let out = ::core::future::poll_fn(move |cx| {
            let mut all = true;
            $( {
                let ( $($s,)* fut, .. ) = &mut futures;
                all &= fut.poll_step(cx);
            } )*
            if !all {
                return ::core::task::Poll::Pending;
            }
            ::core::task::Poll::Ready(( $( {
                let ( $($s,)* fut, .. ) = &mut futures;
                fut.take()
            }, )* ))
        })
        .await;
        $crate::__private::hop(()).await;
        out
    }};
    ($($fut:expr),+ $(,)?) => {
        $crate::join!(@acc [] [] $($fut,)+)
    };
}

/// Awaits several futures with the same output type and returns the output of the first
/// to complete, dropping (cancelling) the others, like SV `fork … join_any; disable fork`.
/// Must be used inside an `async` context. Ties go to the leftmost operand.
///
/// Like cocotb's `First`, the caller resumes after the other tasks ready when the winner
/// completes, and one hop later still if a loser had to be cancelled while waiting (see
/// the module docs).
///
/// ```
/// # use simjig::{FakeModel, Precision, Sim, SimDuration, Timer, first};
/// let mut sim = Sim::from_fake(FakeModel::new(Precision::NS));
/// let out = sim.run(async {
///     first!(
///         async { Timer::new(SimDuration::from_ticks(5)).await; "slow" },
///         async { Timer::new(SimDuration::from_ticks(2)).await; "fast" },
///     )
/// });
/// assert_eq!(out.unwrap(), "fast");
/// ```
#[macro_export]
macro_rules! first {
    ($($fut:expr),+ $(,)?) => {{
        let mut futures = [ $( $crate::__private::boxed($fut), )+ ];
        let out = ::core::future::poll_fn(|cx| {
            for future in futures.iter_mut() {
                if let ::core::task::Poll::Ready(v) = future.as_mut().poll(cx) {
                    return ::core::task::Poll::Ready(v);
                }
            }
            ::core::task::Poll::Pending
        })
        .await;
        $crate::__private::hop(futures).await;
        out
    }};
}
