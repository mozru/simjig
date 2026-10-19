//! Forks nested two combinators deep are refused, and the process that returns is always
//! the parent.

use simjig::fork::{self, ForkError};
use simjig::{Event, TestCtx, Timer, ns, with_timeout};

use crate::counter;

fn assert_in_combinator(result: Result<(), ForkError>) {
    let error = result.unwrap_err();
    assert!(matches!(error, ForkError::InCombinator { .. }), "{error:?}");
}

/// `first!` around `join!` around the fork: the inner operand is a helper of a helper.
#[simjig::test(timeout = "100us")]
async fn a_fork_in_a_join_in_a_first_is_refused(ctx: &TestCtx, dut: counter::Dut) {
    let seed = ctx.seed();
    let pid = std::process::id();
    let r = simjig::first!(
        async {
            Timer::new(ns(10)).await;
            None
        },
        async {
            let ((), r) = simjig::join!(
                async {},
                fork::run_from_checkpoint(seed, 2, |_| async move {
                    dut.clk.rising().await;
                    Ok(())
                })
            );
            Some(r)
        }
    );
    assert_eq!(std::process::id(), pid);
    assert_in_combinator(r.expect("refused in the first poll").map(|_| ()));
}

/// `with_timeout` around `first!` around the fork, with a sibling that never finishes.
#[simjig::test(timeout = "100us")]
async fn a_fork_in_a_first_in_a_with_timeout_is_refused(ctx: &TestCtx) {
    let seed = ctx.seed();
    let pid = std::process::id();
    let never = Event::new("never");
    let r = with_timeout(ns(50), async {
        simjig::first!(
            async {
                never.wait().await;
                None
            },
            async { Some(fork::run_from_checkpoint(seed, 2, |_| async { Ok(()) }).await) }
        )
    })
    .await
    .unwrap();
    assert_eq!(std::process::id(), pid);
    assert_in_combinator(r.expect("the fork operand wins").map(|_| ()));
}

/// Inside a `scope` the caller is not the main task.
#[simjig::test(timeout = "100us")]
async fn a_fork_in_a_scope_is_refused(ctx: &TestCtx) {
    let seed = ctx.seed();
    let r = simjig::scope("s", async move {
        fork::run_from_checkpoint(seed, 2, |_| async { Ok(()) })
            .await
            .map(|_| ())
    })
    .await;
    assert!(matches!(r, Err(ForkError::NotMainTask { .. })), "{r:?}");
}
