//! A traced model forked from a checkpoint: the parent's waves are untouched by its
//! children, and each branch's waves continue in a file of its own
//! (`<stem>.branch-<i>.fst`), complete although the child exits without dropping the model,
//! even when the branch poisons it (a DPI panic, a `$fatal`).

use std::path::PathBuf;

use simjig::fork;
use simjig::{
    DutModel, ModelOptions, ReadOnly, Seed, SeedSource, SimTime, TraceKind, TraceOptions, clock,
    now, ns,
};

use crate::fst;
use crate::{counter_fst, dpi_fst, fatal_fst};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("waves-fork");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    for file in [
        format!("{name}.fst"),
        format!("{name}.branch-0.fst"),
        format!("{name}.branch-1.fst"),
    ] {
        std::fs::remove_file(dir.join(file)).ok();
    }
    dir.join(format!("{name}.fst"))
}

#[test]
fn a_traced_model_forked_into_two_branches_writes_one_fst_per_process() {
    let path = scratch("forked");
    let seed = Seed::new(1, SeedSource::Env);
    let options = ModelOptions::new(seed).with_trace(TraceOptions::new(&path));
    let mut sim = counter_fst::Dut::instantiate(&options).expect("loads");
    let dut = counter_fst::Dut::create(&mut sim).expect("binds");
    let (fork_time, branch_ends) = sim
        .run(async move {
            clock(dut.clk, ns(10)).start();
            dut.rst.set(1);
            dut.en.set(1);
            dut.clk.falling().await;
            dut.rst.set(0);
            for _ in 0..5 {
                dut.clk.rising().await;
            }
            let fork_time = now();
            let branches = fork::run_from_checkpoint(seed, 2, |branch| async move {
                // Branch 0 runs 3 more cycles, branch 1 runs 8.
                for _ in 0..3 + 5 * branch.index() {
                    dut.clk.rising().await;
                }
                ReadOnly::new().await;
                Ok(now())
            })
            .await
            .expect("forks");
            // The parent goes on past both branches.
            for _ in 0..12 {
                dut.clk.rising().await;
            }
            (fork_time, branches.into_values())
        })
        .expect("passes");
    let steps: Vec<u64> = sim
        .recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect();
    drop(sim);

    let parent = fst::read(&path);
    assert_eq!(
        parent.times, steps,
        "the parent's waves are its own timesteps"
    );
    assert_eq!(parent.end, *steps.last().expect("timesteps"));

    assert!(branch_ends[0] < branch_ends[1], "{branch_ends:?}");
    for (index, end) in branch_ends.iter().enumerate() {
        let file = path.with_file_name(format!("forked.branch-{index}.fst"));
        let branch = fst::read(&file);
        assert_eq!(branch.timescale, -12);
        assert_eq!(
            branch.times.first().copied(),
            Some(fork_time.ticks()),
            "branch {index} starts at the checkpoint"
        );
        assert_eq!(
            branch.end,
            end.ticks(),
            "branch {index} ends where it returned"
        );
        assert!(
            branch.times.windows(2).all(|w| w[0] < w[1]),
            "branch {index}: {:?}",
            branch.times
        );
        assert!(branch.end < parent.end);
    }
    assert!(fork_time > SimTime::ZERO);
}

/// Panics when the design adds 7 to anything.
struct NoSevens;

impl dpi_fst::Dpi for NoSevens {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        assert_ne!(x, 7, "seven is not allowed");
        x + y
    }

    fn dpi_tick(&mut self) -> i32 {
        0
    }
}

/// The only branch's failure: its kind and time.
fn branch_failure<T: std::fmt::Debug>(branches: &fork::Branches<T>) -> (String, SimTime) {
    match &branches.outcomes()[0].result {
        Err(fork::BranchError::Failed(failure)) => (failure.kind.clone(), failure.time),
        other => panic!("the branch should fail, got {other:?}"),
    }
}

#[test]
fn a_branch_whose_dpi_import_panics_still_finishes_its_waves() {
    let path = scratch("dpi_panic");
    let seed = Seed::new(1, SeedSource::Env);
    let options = ModelOptions::new(seed).with_trace(TraceOptions::new(&path));
    let mut sim = dpi_fst::Dut::instantiate(&options).expect("loads");
    let dut = dpi_fst::Dut::create(&mut sim).expect("binds");
    dut.set_dpi(NoSevens);
    let (fork_time, (kind, failed_at)) = sim
        .run(async move {
            clock(dut.clk, ns(10)).start();
            dut.a.set(1);
            dut.b.set(2);
            for _ in 0..3 {
                dut.clk.rising().await;
            }
            let fork_time = now();
            let branches = fork::run_from_checkpoint(seed, 1, |_| async move {
                dut.clk.rising().await;
                dut.a.set(7);
                for _ in 0..3 {
                    dut.clk.rising().await;
                }
                Ok(())
            })
            .await
            .expect("forks");
            (fork_time, branch_failure(&branches))
        })
        .expect("the parent passes");
    assert_eq!(kind, "DpiPanic");

    let branch = fst::read(&path.with_file_name("dpi_panic.branch-0.fst"));
    assert_eq!(branch.timescale, -12);
    assert_eq!(branch.times.first().copied(), Some(fork_time.ticks()));
    assert_eq!(
        branch.end,
        failed_at.ticks(),
        "the C++ model is healthy after a DPI panic, so the failing timestep is in the waves"
    );
}

#[test]
fn a_branch_that_hits_a_fatal_still_finishes_its_waves() {
    let path = scratch("fatal");
    let seed = Seed::new(1, SeedSource::Env);
    let options = ModelOptions::new(seed).with_trace(TraceOptions::new(&path));
    let mut sim = fatal_fst::Dut::instantiate(&options).expect("loads");
    let dut = fatal_fst::Dut::create(&mut sim).expect("binds");
    let (fork_time, (kind, failed_at)) = sim
        .run(async move {
            clock(dut.clk, ns(10)).start();
            dut.rst.set(1);
            dut.clk.rising().await;
            dut.clk.falling().await;
            dut.rst.set(0);
            let fork_time = now();
            let branches = fork::run_from_checkpoint(seed, 1, |_| async move {
                dut.clk.rising().await;
                dut.kind.set(0);
                dut.fire.set(1);
                for _ in 0..3 {
                    dut.clk.rising().await;
                }
                Ok(())
            })
            .await
            .expect("forks");
            (fork_time, branch_failure(&branches))
        })
        .expect("the parent passes");
    assert_eq!(kind, "ModelFatal");

    let branch = fst::read(&path.with_file_name("fatal.branch-0.fst"));
    assert_eq!(branch.timescale, -12);
    assert_eq!(branch.times.first().copied(), Some(fork_time.ticks()));
    // The `$fatal`'s own timestep is not in the waves (the poisoned model refuses the
    // dump); the timestep before it, the falling edge 5 ns earlier, is the last sample.
    assert_eq!(branch.end, failed_at.ticks() - 5_000);
    assert!(branch.times.windows(2).all(|w| w[0] < w[1]));
}
