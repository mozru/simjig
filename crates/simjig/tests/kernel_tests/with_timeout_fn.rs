//! `with_timeout` as a plain `#[track_caller]` fn.

use std::pin::pin;

use simjig::{Failure, timer, with_timeout};

use super::{dut, ticks};

/// `&mut fut` (pinned) still works, the future can be retried after a timeout, and a
/// `with_timeout` future built outside the async block keeps its caller's location.
#[test]
fn with_timeout_over_a_borrowed_pinned_future_resumes_it() {
    let mut d = dut();
    let out = d.sim.run(async move {
        let mut slow = pin!(async {
            timer(ticks(10)).await;
            7
        });
        assert!(with_timeout(ticks(3), &mut slow).await.is_err());
        let built_early = with_timeout(ticks(100), &mut slow);
        built_early.await.unwrap()
    });
    assert_eq!(out.unwrap(), 7);
}

/// `?` on an `Elapsed` from a stored future: the failure points at the `?`.
#[test]
fn timeout_failure_location_with_a_stored_future() {
    let mut d = dut();
    let line = line!() + 5;
    let f = d
        .sim
        .run(async move {
            let fut = with_timeout(ticks(1), timer(ticks(5)));
            fut.await?;
            Ok::<(), Failure>(())
        })
        .unwrap()
        .unwrap_err();
    assert!(f.location.is_some_and(|l| l.line() == line), "{f:?}");
}

/// README: `&mut task` for a Task that must keep running past the timeout.
#[test]
fn with_timeout_over_mut_task_keeps_it_running() {
    let mut d = dut();
    let out = d.sim.run(async move {
        let mut t = simjig::spawn("slow", async {
            timer(ticks(10)).await;
            9
        });
        assert!(with_timeout(ticks(3), &mut t).await.is_err());
        t.await
    });
    assert_eq!(out.unwrap(), 9);
}

/// `.expect()` on a timeout prints `Elapsed`'s `Debug`: physical units, not raw ticks.
#[test]
fn elapsed_debug_prints_physical_units() {
    let mut d = dut();
    let shown = d
        .sim
        .run(async move {
            timer(ticks(2)).await;
            let e = with_timeout(ticks(3), timer(ticks(10))).await.unwrap_err();
            format!("{e:?}")
        })
        .unwrap();
    assert_eq!(shown, "Elapsed { after: 3 ns, at: 5 ns, precision: 1ns }");
}
