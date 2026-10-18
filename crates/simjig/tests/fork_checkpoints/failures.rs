//! A failing branch is reported with its index and seed, whatever ended it; the parent
//! and the other branches carry on.

use simjig::fork::{self, BranchError, ChildExit};
use simjig::{ReadOnly, TestCtx, clock, ns};
use simjig_bits::Bits;

use crate::fatal;

/// Branch behaviour by index.
const OK: usize = 0;
const PANICS: usize = 1;
const MODEL_FATAL: usize = 2;
const TIME_LIMIT: usize = 3;
const ABORTS: usize = 4;

/// Five branches from one post-reset checkpoint, each ending in its own way.
async fn five_endings(ctx: &TestCtx, dut: fatal::Dut) -> fork::Branches<u8> {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    fork::run_from_checkpoint(ctx.seed(), 5, |branch| async move {
        match branch.index() {
            PANICS => {
                dut.clk.rising().await;
                assert_eq!(dut.alive.get(), Bits::new(99), "branch {PANICS} checks");
            }
            MODEL_FATAL => {
                dut.kind.set(0);
                dut.fire.set(1);
                for _ in 0..3 {
                    dut.clk.rising().await;
                }
            }
            TIME_LIMIT => loop {
                dut.clk.rising().await;
            },
            ABORTS => std::process::abort(),
            _ => {
                for _ in 0..4 {
                    dut.clk.rising().await;
                }
                ReadOnly::new().await;
            }
        }
        Ok(u8::try_from(dut.alive.get().to_u128()).unwrap())
    })
    .await
    .unwrap()
}

#[simjig::test(timeout = "10us")]
async fn each_failure_names_its_branch_kind_and_seed(ctx: &TestCtx, dut: fatal::Dut) {
    let branches = five_endings(ctx, dut).await;
    let outcomes = branches.outcomes();
    assert_eq!(outcomes.len(), 5);
    for (i, outcome) in outcomes.iter().enumerate() {
        assert_eq!(outcome.branch.index(), i);
        assert_eq!(outcome.branch.count(), 5);
    }
    assert_eq!(outcomes[OK].result, Ok(4));

    let failed = |i: usize| match &outcomes[i].result {
        Err(BranchError::Failed(failure)) => failure.clone(),
        other => panic!("branch {i}: {other:?}"),
    };
    let panic = failed(PANICS);
    assert_eq!(panic.kind, "TaskPanic");
    assert_eq!(panic.task.as_deref(), Some("main"));
    assert!(panic.message.contains("branch 1 checks"), "{panic:?}");
    assert!(
        panic.location.as_deref().unwrap().contains("failures.rs"),
        "{panic:?}"
    );
    let fatal = failed(MODEL_FATAL);
    assert_eq!(fatal.kind, "ModelFatal");
    assert!(fatal.message.contains("fatal: kind=0"), "{fatal:?}");
    assert_eq!(failed(TIME_LIMIT).kind, "TimeLimit");
    assert_eq!(
        outcomes[ABORTS].result,
        Err(BranchError::Died(ChildExit::Signaled(SIGABRT)))
    );

    let reports: Vec<String> = branches.failures().map(|f| f.to_string()).collect();
    assert_eq!(reports.len(), 4);
    let panic_branch = outcomes[PANICS].branch;
    assert!(
        reports[0].starts_with(&format!(
            "fork branch 1 of 5 (seed {}) failed: simjig TaskPanic",
            panic_branch.seed()
        )),
        "{}",
        reports[0]
    );
    assert!(
        reports[3].ends_with("died without a report: killed by signal 6"),
        "{}",
        reports[3]
    );

    // The parent is unaffected: it is still right after reset, and still runs.
    assert_eq!(dut.alive.get(), Bits::new(0));
    dut.clk.rising().await;
    dut.clk.falling().await;
    assert_eq!(dut.alive.get(), Bits::new(1));
}

/// The signal `abort()` raises (6 on Linux and macOS).
const SIGABRT: i32 = 6;

/// `into_values` fails the test, listing each failed branch with its index and seed.
#[simjig::test(timeout = "10us")]
#[should_panic(expected = "fork branch 1 of 5 (seed 0x")]
async fn into_values_fails_the_test_naming_every_failed_branch(ctx: &TestCtx, dut: fatal::Dut) {
    let _ = five_endings(ctx, dut).await.into_values();
}
