//! A traced model forked from a checkpoint: the parent's waves are untouched by its
//! children, and each branch's waves continue in a file of its own
//! (`<stem>.branch-<i>.fst`), complete although the child exits without dropping the model.

use std::path::PathBuf;

use simjig::fork;
use simjig::{
    DutModel, ModelOptions, ReadOnly, Seed, SeedSource, SimTime, TraceKind, TraceOptions, clock,
    now, ns,
};

use crate::counter_fst;
use crate::fst;

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
