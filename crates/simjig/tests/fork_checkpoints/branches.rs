//! Branches diverge from one checkpoint, and the parent continues as if none had run.

use simjig::fork::{self, BranchError};
use simjig::{
    DutModel, ModelOptions, ReadOnly, Seed, SeedSource, SimTime, TestCtx, clock, now, ns,
};
use simjig_bits::Bits;

use crate::counter;

/// Reset, then count to 10: the checkpoint every test here starts from.
async fn count_to_ten(dut: counter::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(0);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    dut.en.set(1);
    for _ in 0..10 {
        dut.clk.rising().await;
    }
    dut.clk.falling().await;
}

/// `cycles` more rising edges with `en` set.
async fn count(dut: counter::Dut, cycles: u32) {
    dut.en.set(1);
    for _ in 0..cycles {
        dut.clk.rising().await;
    }
    ReadOnly::new().await;
}

#[simjig::test(timeout = "100us")]
async fn branches_diverge_and_the_parent_is_unaffected(ctx: &TestCtx, dut: counter::Dut) {
    count_to_ten(dut).await;
    let at_fork = (dut.count.get(), now());
    let branches = fork::run_from_checkpoint(ctx.seed(), 4, |branch| async move {
        let start = (dut.count.get(), now());
        // Branch i counts 3i more cycles; branch 3 resets first.
        if branch.index() == 3 {
            dut.rst.set(1);
            dut.clk.rising().await;
            dut.clk.falling().await;
            dut.rst.set(0);
        }
        let more = u32::try_from(3 * branch.index()).unwrap();
        count(dut, more).await;
        Ok((start, dut.count.get(), now()))
    })
    .await
    .unwrap();

    assert_eq!(branches.outcomes().len(), 4);
    for outcome in branches.outcomes() {
        let (start, _, _) = outcome.result.as_ref().unwrap();
        assert_eq!(*start, at_fork, "every branch starts at the checkpoint");
    }
    let ends: Vec<u128> = branches
        .into_values()
        .into_iter()
        .map(|(_, count, _)| count.to_u128())
        .collect();
    assert_eq!(ends, [10, 13, 16, 9]);

    // The parent is still at the checkpoint, and carries on from it.
    assert_eq!((dut.count.get(), now()), at_fork);
    count(dut, 5).await;
    assert_eq!(dut.count.get(), Bits::<8>::new(15));
}

/// The checkpoint changes nothing the kernel records: a run that forks has the same trace
/// hash, time and result as one that does not.
#[test]
fn the_parent_matches_a_run_that_never_forked() {
    let run = |fork_here: bool| {
        let options = ModelOptions::new(Seed::new(7, SeedSource::Env));
        let mut sim = counter::Dut::instantiate(&options).unwrap();
        let dut = counter::Dut::create(&mut sim).unwrap();
        let seed = options.seed;
        let end = sim
            .run(async move {
                count_to_ten(dut).await;
                if fork_here {
                    let branches = fork::run_from_checkpoint(seed, 2, |_| async move {
                        count(dut, 100).await;
                        Ok(dut.count.get())
                    })
                    .await
                    .unwrap();
                    assert_eq!(branches.into_values(), [Bits::new(110); 2]);
                }
                count(dut, 7).await;
                dut.count.get()
            })
            .unwrap();
        (end, sim.now(), sim.trace_hash())
    };
    let forked = run(true);
    assert_eq!(forked.0, Bits::<8>::new(17));
    assert_eq!(forked, run(false));
    assert!(forked.1 > SimTime(0));
}

#[simjig::test(timeout = "100us")]
async fn branch_seeds_are_distinct_and_follow_the_root_seed(ctx: &TestCtx, dut: counter::Dut) {
    count_to_ten(dut).await;
    let draw = |root| async move {
        fork::run_from_checkpoint(root, 3, |branch| async move {
            Ok((branch.seed().value(), branch.rng("stim").next_u64()))
        })
        .await
        .unwrap()
        .into_values()
    };
    let first = draw(ctx.seed()).await;
    assert_eq!(
        first,
        draw(ctx.seed()).await,
        "same root, same branch seeds"
    );
    assert_ne!(first[0].0, first[1].0);
    assert_ne!(first[1].0, first[2].0);
    let other = draw(Seed::new(ctx.seed().value() ^ 1, SeedSource::Env)).await;
    assert_ne!(first, other, "another root, other branch seeds");
}

#[simjig::test(timeout = "100us")]
async fn a_branch_that_returns_an_error_is_a_failed_branch(ctx: &TestCtx, dut: counter::Dut) {
    count_to_ten(dut).await;
    let branches = fork::run_from_checkpoint(ctx.seed(), 1, |_| async move {
        let failure = simjig::Failure {
            kind: simjig::FailureKind::DesignFinished,
            time: SimTime(1),
            precision: simjig::Precision::PS,
            task: None,
            location: None,
            message: "returned by the body".into(),
        };
        simjig::Result::<()>::Err(failure)
    })
    .await
    .unwrap();
    let Err(BranchError::Failed(failure)) = &branches.outcomes()[0].result else {
        panic!("{:?}", branches.outcomes()[0].result);
    };
    assert_eq!(failure.kind, "DesignFinished");
    assert_eq!(failure.message, "returned by the body");
}

/// The open descriptors of this process, by number.
fn open_descriptors() -> Vec<String> {
    let mut fds: Vec<String> = std::fs::read_dir("/dev/fd")
        .unwrap_or_else(|e| panic!("/dev/fd lists this process's descriptors: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    fds.sort();
    fds
}

/// Each branch's socket is closed in the parent and every child is reaped: a checkpoint
/// leaves no descriptor open and no zombie process.
#[simjig::test(timeout = "1us")]
async fn branches_leave_no_descriptor_or_process_behind(ctx: &TestCtx, dut: counter::Dut) {
    count_to_ten(dut).await;
    let before = open_descriptors();
    let branches = fork::run_from_checkpoint(ctx.seed(), 3, |_| async move {
        dut.clk.rising().await;
        Ok(u64::from(std::process::id()))
    })
    .await
    .unwrap();
    assert_eq!(open_descriptors(), before, "the parent's descriptors");
    for pid in branches.into_values() {
        let ps = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps runs");
        let state = String::from_utf8_lossy(&ps.stdout);
        assert!(state.trim().is_empty(), "child {pid} still exists: {state}");
    }
}
