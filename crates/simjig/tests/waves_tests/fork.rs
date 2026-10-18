//! A traced model forked from a checkpoint: the parent's waves are untouched by its
//! children, and each branch's waves continue in a file of its own
//! (`<stem>.branch-<i>.fst`), complete although the child exits without dropping the model,
//! even when the branch poisons it (a DPI panic, a `$fatal`). A child that cannot set up
//! its branch (a fork hook fails, the branch file cannot be created) never writes the
//! parent's files either.

use std::path::{Path, PathBuf};

use simjig::fork::{self, ForkHook};
use simjig::{
    DutModel, ModelOptions, ReadOnly, Seed, SeedSource, SimTime, TraceKind, TraceOptions, clock,
    now, ns,
};

use crate::fst;
use crate::{counter_fst, dpi_fst, fatal_fst};

/// `<scratch>/<name>.fst`, with every earlier `<name>.*` file or directory removed.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("waves-fork");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let prefix = format!("{name}.");
    for entry in std::fs::read_dir(&dir).expect("the scratch directory") {
        let entry = entry.expect("a directory entry").path();
        let ours = entry
            .file_name()
            .is_some_and(|f| f.to_string_lossy().starts_with(&prefix));
        if ours && entry.is_dir() {
            std::fs::remove_dir_all(&entry).expect("removes an old directory");
        } else if ours {
            std::fs::remove_file(&entry).expect("removes an old file");
        }
    }
    dir.join(format!("{name}.fst"))
}

/// The simulation's timesteps, in ticks.
fn steps(sim: &simjig::Sim) -> Vec<u64> {
    sim.recent_trace()
        .iter()
        .filter(|r| r.kind == TraceKind::Step)
        .map(|r| r.time.ticks())
        .collect()
}

/// Makes `name` (next to `path`) uncreatable as a file: a directory takes its place.
fn block(path: &Path, name: &str) {
    std::fs::create_dir(path.with_file_name(name)).expect("a blocking directory");
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
    let steps = steps(&sim);
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

/// Runs the counter traced to `path`, forks one branch of 3 cycles at a checkpoint, then
/// runs 10 more cycles; returns the parent's timesteps and the branch's outcome.
fn counter_with_one_branch(path: &Path) -> (Vec<u64>, Result<(), fork::BranchError>) {
    let seed = Seed::new(1, SeedSource::Env);
    let options = ModelOptions::new(seed).with_trace(TraceOptions::new(path));
    let mut sim = counter_fst::Dut::instantiate(&options).expect("loads");
    let dut = counter_fst::Dut::create(&mut sim).expect("binds");
    let outcome = sim
        .run(async move {
            clock(dut.clk, ns(10)).start();
            dut.rst.set(1);
            dut.en.set(1);
            dut.clk.falling().await;
            dut.rst.set(0);
            for _ in 0..5 {
                dut.clk.rising().await;
            }
            let branches = fork::run_from_checkpoint(seed, 1, |_| async move {
                for _ in 0..3 {
                    dut.clk.rising().await;
                }
                Ok(())
            })
            .await
            .expect("forks");
            for _ in 0..10 {
                dut.clk.rising().await;
            }
            branches.outcomes()[0].result.clone()
        })
        .expect("the parent passes");
    let steps = steps(&sim);
    drop(sim);
    (steps, outcome)
}

/// The message of a branch that failed with a report.
fn failure_message(outcome: &Result<(), fork::BranchError>) -> &str {
    match outcome {
        Err(fork::BranchError::Failed(failure)) => &failure.message,
        other => panic!("the branch should fail with a report, got {other:?}"),
    }
}

/// A user's hook registered before the model fails in the child, so the trace's hook never
/// detaches the inherited writer: the child must then leave that writer alone (closing it
/// would write a header through the descriptor it shares with the parent).
#[test]
fn a_hook_failing_before_the_trace_detaches_leaves_the_parents_fst_alone() {
    let path = scratch("hook_first");
    let _guard = fork::register_hook(ForkHook::new(
        || Ok(()),
        |_| Err("the user's hook fails".to_owned()),
    ));
    let (steps, outcome) = counter_with_one_branch(&path);
    assert!(
        failure_message(&outcome).contains("the user's hook fails"),
        "{outcome:?}"
    );
    let parent = fst::read(&path);
    assert_eq!(
        parent.times, steps,
        "the parent's file holds only its own waves"
    );
    assert_eq!(parent.end, *steps.last().expect("timesteps"));
}

/// The branch's file cannot be created: the reopen fails, and the inherited writer must
/// already be detached (or left alone), never closed onto the parent's file.
#[test]
fn an_uncreatable_branch_file_fails_the_branch_and_leaves_the_parents_fst_alone() {
    let path = scratch("blocked");
    block(&path, "blocked.branch-0.fst");
    let (steps, outcome) = counter_with_one_branch(&path);
    assert!(
        failure_message(&outcome).contains("blocked.branch-0.fst"),
        "{outcome:?}"
    );
    let parent = fst::read(&path);
    assert_eq!(
        parent.times, steps,
        "the parent's file holds only its own waves"
    );
    assert_eq!(parent.end, *steps.last().expect("timesteps"));
}

/// Model 0's branch file cannot be created, so its reopen fails first and model 1's hook
/// never runs: model 1's inherited writer must be left alone.
#[test]
fn an_uncreatable_branch_file_of_model_0_leaves_model_1s_parent_fst_alone() {
    two_models_with_a_blocked_branch_file("multi_a", "multi_a.branch-0.fst");
}

/// Model 1's branch file cannot be created: its own failed reopen must not close its
/// inherited writer onto the parent's `.model-1` file.
#[test]
fn an_uncreatable_branch_file_of_model_1_leaves_its_parent_fst_alone() {
    two_models_with_a_blocked_branch_file("multi_b", "multi_b.model-1.branch-0.fst");
}

/// Two traced counters forked once with the branch file `blocked` uncreatable: the branch
/// fails naming it, and both parent files hold exactly the parent's timesteps.
fn two_models_with_a_blocked_branch_file(name: &str, blocked: &str) {
    let path = scratch(name);
    block(&path, blocked);
    let seed = Seed::new(1, SeedSource::Env);
    let options = ModelOptions::new(seed).with_trace(TraceOptions::new(&path));
    let mut sim = <(counter_fst::Dut, counter_fst::Dut)>::instantiate(&options).expect("loads");
    let (a, b) = <(counter_fst::Dut, counter_fst::Dut)>::create(&mut sim).expect("binds");
    let outcome = sim
        .run(async move {
            clock(a.clk, ns(10)).start();
            clock(b.clk, ns(10)).start();
            for dut in [a, b] {
                dut.rst.set(1);
                dut.en.set(1);
            }
            a.clk.falling().await;
            a.rst.set(0);
            b.rst.set(0);
            for _ in 0..5 {
                a.clk.rising().await;
            }
            let branches = fork::run_from_checkpoint(seed, 1, |_| async move {
                for _ in 0..3 {
                    a.clk.rising().await;
                }
                Ok(())
            })
            .await
            .expect("forks");
            for _ in 0..10 {
                a.clk.rising().await;
            }
            branches.outcomes()[0].result.clone()
        })
        .expect("the parent passes");
    let steps = steps(&sim);
    drop(sim);
    assert!(failure_message(&outcome).contains(blocked), "{outcome:?}");
    for file in [format!("{name}.fst"), format!("{name}.model-1.fst")] {
        let parent = fst::read(&path.with_file_name(&file));
        assert_eq!(
            parent.times, steps,
            "{blocked}: {file} holds only its own waves"
        );
        assert_eq!(parent.end, *steps.last().expect("timesteps"), "{file}");
    }
}
