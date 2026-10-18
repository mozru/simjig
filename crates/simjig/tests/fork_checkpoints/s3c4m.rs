//! Fork checkpoints against stage 2's helper-task combinators and Event/Lock hops (from
//! the s3c4 integration review): a fork inside a combinator operand is refused, and a
//! branch child never returns into the harness.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::fork::{self, ForkError};
use simjig::{Event, Lock, ReadOnly, TestCtx, Timer, clock, now, ns, spawn, with_timeout};

use crate::counter;

async fn warm_up(dut: counter::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(0);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    dut.en.set(1);
    for _ in 0..3 {
        dut.clk.rising().await;
    }
    dut.clk.falling().await;
}

/// Combinators, a spawned task, Event and Lock hops: returns an ordered log of what ran.
async fn seam_body(dut: counter::Dut, extra: u32) -> Vec<String> {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let ev = Event::new("ev");
    let lock = Lock::new("lock");
    // first!: a 1 ns timer beats the next rising edge (5 ns away).
    let w = simjig::first!(
        async {
            dut.clk.rising().await;
            1u32
        },
        async {
            Timer::new(ns(1)).await;
            2u32
        }
    );
    log.borrow_mut()
        .push(format!("first={w} t={}", now().ticks()));
    // A set event and a free lock: each a hop behind the ready tasks.
    let l2 = Rc::clone(&log);
    let other = spawn("other", async move {
        l2.borrow_mut().push("other".into());
    });
    ev.set();
    with_timeout(ns(100), ev.wait()).await.unwrap();
    log.borrow_mut().push("ev".into());
    {
        let _g = lock.lock().await;
        log.borrow_mut().push("locked".into());
        let (a, b) = simjig::join!(
            async {
                for _ in 0..extra {
                    dut.clk.rising().await;
                }
                ReadOnly::new().await;
                dut.count.get().to_u128()
            },
            async {
                // The lock is held: this times out.
                with_timeout(ns(3), lock.lock()).await.is_err()
            }
        );
        log.borrow_mut()
            .push(format!("count={a} lock_timed_out={b}"));
    }
    other.await;
    log.borrow_mut().push(format!("end t={}", now().ticks()));
    Rc::try_unwrap(log).unwrap().into_inner()
}

/// Each branch running the body matches the parent running the same body afterwards.
#[simjig::test(timeout = "100us")]
async fn s3c4m_combinators_and_hops_in_a_branch_match_the_parent(ctx: &TestCtx, dut: counter::Dut) {
    warm_up(dut).await;
    let branches = fork::run_from_checkpoint(ctx.seed(), 3, |b| async move {
        Ok(seam_body(dut, u32::try_from(b.index()).unwrap() + 1).await)
    })
    .await
    .unwrap();
    let values = branches.into_values();
    let parent = seam_body(dut, 1).await;
    eprintln!("branch0={:?}\nparent={parent:?}", values[0]);
    assert_eq!(
        values[0], parent,
        "branch 0 and the parent run the same body"
    );
    assert_ne!(values[1], values[0]);
}

/// A fork from inside a combinator operand (polled as a helper of `main`) is refused.
#[simjig::test(timeout = "100us")]
async fn s3c4m_a_fork_inside_with_timeout_is_refused(ctx: &TestCtx, dut: counter::Dut) {
    warm_up(dut).await;
    let seed = ctx.seed();
    let result = with_timeout(
        ns(100),
        fork::run_from_checkpoint(seed, 2, |_| async { Ok(()) }),
    )
    .await
    .unwrap();
    assert_refused(result.map(|_| ()));
}

/// The refusal names combinators and says where to call from instead.
fn assert_refused(result: Result<(), ForkError>) {
    let error = result.unwrap_err();
    assert!(matches!(error, ForkError::InCombinator { .. }), "{error:?}");
    let text = error.to_string();
    assert!(
        text.contains("`join!`, `first!` or `with_timeout`"),
        "{text}"
    );
    assert!(text.contains("outside every combinator"), "{text}");
}

/// Right after a `first!` whose loser was cancelled, no helper is left: the fork is quiet.
#[simjig::test(timeout = "100us")]
async fn s3c4m_after_a_first_the_checkpoint_is_quiet(ctx: &TestCtx, dut: counter::Dut) {
    warm_up(dut).await;
    let ev = Event::new("never");
    let w = simjig::first!(
        async {
            ev.wait().await;
            0u32
        },
        async {
            dut.clk.rising().await;
            1u32
        }
    );
    assert_eq!(w, 1);
    let branches = fork::run_from_checkpoint(ctx.seed(), 2, |_| async move {
        dut.clk.rising().await;
        Ok(dut.count.get())
    })
    .await
    .unwrap();
    assert_eq!(branches.into_values().len(), 2);
}

/// A sibling operand of a `join!` is a helper, not a task: it would keep running inside
/// the branch next to the body, so the fork is refused.
#[simjig::test(timeout = "100us")]
async fn s3c4m_a_fork_beside_a_join_sibling(ctx: &TestCtx, dut: counter::Dut) {
    warm_up(dut).await;
    let seed = ctx.seed();
    let edges = Rc::new(std::cell::Cell::new(0u32));
    let e2 = Rc::clone(&edges);
    let ((), result) = simjig::join!(
        async move {
            for _ in 0..3 {
                dut.clk.rising().await;
                e2.set(e2.get() + 1);
            }
        },
        fork::run_from_checkpoint(seed, 1, |_| {
            let e3 = Rc::clone(&edges);
            async move {
                for _ in 0..5 {
                    dut.clk.rising().await;
                }
                Ok(e3.get())
            }
        })
    );
    assert_refused(result.map(|_| ()));
    assert_eq!(edges.get(), 3, "the sibling ran to its end in the parent");
}

/// A `first!` sibling timer that would win inside the branch (cancelling the body, and
/// leaving the child to run the rest of the test as the parent) is ruled out: refused.
#[simjig::test(timeout = "100us")]
async fn s3c4m_a_fork_whose_first_sibling_wins_in_the_child(ctx: &TestCtx, dut: counter::Dut) {
    warm_up(dut).await;
    let seed = ctx.seed();
    let pid = std::process::id();
    let r = simjig::first!(
        async {
            Timer::new(ns(10)).await;
            None
        },
        async {
            Some(
                fork::run_from_checkpoint(seed, 1, |_| async move {
                    Timer::new(ns(50)).await;
                    Ok(())
                })
                .await,
            )
        }
    );
    assert_eq!(std::process::id(), pid, "a child ran the rest of the test");
    let result = r.expect("the refusal completes in the operand's first poll");
    assert_refused(result.map(|_| ()));
}

/// A branch child whose `run_from_checkpoint` future is dropped before the body returns
/// (here by a hand-written poll, the only way left once combinators are refused) ends
/// with a failure, and never runs the rest of the test.
#[simjig::test(timeout = "100us")]
async fn a_branch_whose_fork_future_is_dropped_ends_the_child(ctx: &TestCtx, dut: counter::Dut) {
    warm_up(dut).await;
    let pid = std::process::id();
    let mut fork = Box::pin(fork::run_from_checkpoint(ctx.seed(), 1, |_| async move {
        Timer::new(ns(50)).await;
        Ok(())
    }));
    // The parent's first poll returns the outcomes; the child's is pending on the timer.
    let polled = std::future::poll_fn(|cx| std::task::Poll::Ready(fork.as_mut().poll(cx))).await;
    drop(fork);
    assert_eq!(
        std::process::id(),
        pid,
        "the child ran on after its fork future was dropped"
    );
    let std::task::Poll::Ready(result) = polled else {
        panic!("the parent's poll of run_from_checkpoint is pending");
    };
    let branches = result.unwrap();
    let Err(fork::BranchError::Failed(failure)) = &branches.outcomes()[0].result else {
        panic!("{:?}", branches.outcomes()[0].result);
    };
    assert!(
        failure
            .message
            .contains("the branch body was dropped before it returned"),
        "{failure:?}"
    );
}
