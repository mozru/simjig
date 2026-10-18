//! The rendered failure block, one snapshot per `FailureKind`.

use std::path::PathBuf;

use simjig::fork;
use simjig::{
    AppliedTimeout, BlockedTask, Failure, FailureKind, FailureReport, Precision, RunIdentity, Seed,
    SeedSource, SimDuration, SimTime, Verdict, WavesFile, WavesReplay,
};

fn run() -> RunIdentity {
    RunIdentity {
        test: "fifo::backpressure".into(),
        package: "fifo_tb".into(),
        seed: Seed::new(0x9e37_79b9_7f4a_7c15, SeedSource::Env),
        ignored: false,
        launcher: None,
        binary: None,
    }
}

fn failure(kind: FailureKind, task: Option<&str>, message: &str) -> Failure {
    Failure {
        kind,
        time: SimTime(1250),
        precision: Precision::NS,
        task: task.map(Into::into),
        location: None,
        message: message.into(),
    }
}

fn render(failure: &Failure, location: Option<&str>) -> String {
    FailureReport::new(failure, run(), location.map(Into::into), None).to_string()
}

#[test]
fn task_panic() {
    let f = failure(
        FailureKind::TaskPanic {
            message: "expected 0x12, got 0x13\nat beat 4".into(),
        },
        Some("main.driver"),
        "panicked: expected 0x12, got 0x13\nat beat 4",
    );
    insta::assert_snapshot!(render(&f, Some("tests/fifo.rs:42:9")));
}

#[test]
fn delta_limit() {
    let f = failure(
        FailureKind::DeltaLimit {
            limit: 64,
            signals: vec!["top.a".into(), "top.b".into()],
        },
        None,
        "the timestep did not settle within 64 evals",
    );
    insta::assert_snapshot!(render(&f, None));
}

#[test]
fn write_conflict() {
    let f = failure(
        FailureKind::WriteConflict {
            signal: "top.valid".into(),
            // The kernel's writer labels, quoted by the kernel.
            first: "task `main.a`".into(),
            second: "the clock on `top.valid`".into(),
        },
        Some("main.b"),
        "signal `top.valid` written with different values by task `main.a` and by the clock \
         on `top.valid` in the same delta",
    );
    let text = render(&f, Some("tests/fifo.rs:7:5"));
    assert!(
        text.contains("  writers:  task `main.a`, then the clock on `top.valid`\n"),
        "{text}"
    );
    insta::assert_snapshot!(text);
}

#[test]
fn the_kernel_location_is_used_without_an_override() {
    let location = std::panic::Location::caller();
    let f = Failure {
        location: Some(location.into()),
        ..failure(
            FailureKind::WriteInReadOnly {
                signal: "top.data".into(),
            },
            None,
            "written",
        )
    };
    let report = FailureReport::new(&f, run(), None, None);
    assert_eq!(report.location, Some(location.to_string()));
}

#[test]
fn write_in_read_only() {
    let f = failure(
        FailureKind::WriteInReadOnly {
            signal: "top.data".into(),
        },
        Some("main.monitor"),
        "`top.data` written in ReadOnly",
    );
    insta::assert_snapshot!(render(&f, None));
}

#[test]
fn starved() {
    let f = failure(
        FailureKind::Starved {
            blocked: vec![
                BlockedTask {
                    task: "main".into(),
                    awaiting: vec!["rising(clk)".into()],
                },
                BlockedTask {
                    task: "main.sink".into(),
                    awaiting: vec![],
                },
            ],
        },
        None,
        "nothing is scheduled, and 2 tasks are blocked",
    );
    insta::assert_snapshot!(render(&f, None));
}

#[test]
fn deadlock() {
    let f = failure(
        FailureKind::Deadlock {
            blocked: vec![BlockedTask {
                task: "main.a".into(),
                awaiting: vec!["Lock `bus`".into()],
            }],
        },
        None,
        "1 task blocked",
    );
    insta::assert_snapshot!(render(&f, None));
}

#[test]
fn model_fatal() {
    let f = failure(
        FailureKind::ModelFatal {
            message: "fifo.sv:88: overflow".into(),
        },
        None,
        "[1250] %Fatal: fifo.sv:88: top.u_fifo: overflow",
    );
    insta::assert_snapshot!(render(&f, None));
}

#[test]
fn design_finished() {
    let f = failure(
        FailureKind::DesignFinished,
        None,
        "the design called $finish",
    );
    insta::assert_snapshot!(render(&f, None));
}

#[test]
fn time_limit_with_the_run_dir() {
    let f = failure(
        FailureKind::TimeLimit {
            limit: SimTime(1000),
            blocked: vec![BlockedTask {
                task: "main".into(),
                awaiting: vec!["rising(`top.clk`)".into()],
            }],
        },
        None,
        "the next timestep, t=1250 ns, is past the time limit",
    );
    let mut report = FailureReport::new(
        &f,
        run(),
        None,
        Some(AppliedTimeout {
            duration: SimDuration::from_ticks(1000),
            is_default: false,
        }),
    );
    report.run_dir = Some(PathBuf::from(
        "target/simjig/runs/fifo__backpressure/9e3779b97f4a7c15",
    ));
    insta::assert_snapshot!(report.to_string());
}

#[test]
fn the_repro_of_an_ignored_test_runs_ignored_tests() {
    let run = RunIdentity {
        ignored: true,
        ..run()
    };
    assert_eq!(
        run.repro(),
        "SIMJIG_SEED=0x9e3779b97f4a7c15 cargo nextest run -p fifo_tb --run-ignored all \
         -E 'test(=fifo::backpressure)'"
    );
}

#[test]
fn the_repro_names_the_test_binary() {
    // `test(=...)` alone also matches a same-named test in another test file.
    let run = RunIdentity {
        binary: Some("fifo_tests".into()),
        ..run()
    };
    assert_eq!(
        run.repro(),
        "SIMJIG_SEED=0x9e3779b97f4a7c15 cargo nextest run -p fifo_tb \
         -E 'binary(=fifo_tests) & test(=fifo::backpressure)'"
    );
}

#[test]
fn the_signature_is_stable_across_seeds_and_values() {
    let at = |value: u32, time: u64| {
        let message = format!("expected 0x12, got {value:#x}");
        let f = Failure {
            time: SimTime(time),
            ..failure(
                FailureKind::TaskPanic {
                    message: message.clone(),
                },
                Some("main"),
                &message,
            )
        };
        FailureReport::new(&f, run(), Some("tests/fifo.rs:42:9".into()), None).signature
    };
    assert_eq!(at(0x13, 10), at(0x99, 999));
}

#[test]
fn times_print_in_the_largest_exact_unit() {
    let f = Failure {
        time: SimTime(1_000_000),
        precision: Precision::PS,
        ..failure(
            FailureKind::TimeLimit {
                limit: SimTime(250_000),
                blocked: Vec::new(),
            },
            None,
            "past the limit",
        )
    };
    let text = render(&f, None);
    assert!(text.contains("  t=1 us  "), "{text}");
    assert!(text.contains("limit:    t=250 ns\n"), "{text}");
}

#[test]
fn a_replay_lists_every_waves_file_with_the_failing_branch_first() {
    let f = failure(
        FailureKind::TaskPanic {
            message: "1 of 2 fork branches failed".into(),
        },
        Some("main"),
        "panicked: 1 of 2 fork branches failed",
    );
    let mut report = FailureReport::new(&f, run(), None, None);
    let file =
        |name: &str, model: usize, branch: Vec<usize>, marker: u64, failing: bool| WavesFile {
            fst: PathBuf::from(format!("/runs/r/{name}.fst")),
            gtkw: Some(PathBuf::from(format!("/runs/r/{name}.gtkw"))),
            model,
            branch,
            marker: Some(SimTime(marker)),
            failing_branch: failing,
            died: None,
        };
    report.waves = Some(WavesReplay {
        files: vec![
            file("waves.branch-1", 0, vec![1], 1750, true),
            file("waves.model-1.branch-1", 1, vec![1], 1750, true),
            file("waves", 0, vec![], 1250, false),
            file("waves.model-1", 1, vec![], 1250, false),
        ],
        from: SimTime(250),
        to: None,
        precision: Precision::NS,
        verdict: Verdict::Reproduced,
    });
    insta::assert_snapshot!(report.to_string());
}

/// A branch whose child died has no failure time: its file is listed last (the replay
/// sorts it there), labelled incomplete, with no marker, and here (no FST header) with no
/// `gtkwave` command.
#[test]
fn the_file_of_a_dead_fork_branch_is_labelled_incomplete() {
    let f = failure(
        FailureKind::TaskPanic {
            message: "1 of 2 fork branches failed".into(),
        },
        Some("main"),
        "panicked: 1 of 2 fork branches failed",
    );
    let mut report = FailureReport::new(&f, run(), None, None);
    let readable = |name: &str, branch: Vec<usize>| WavesFile {
        fst: PathBuf::from(format!("/runs/r/{name}.fst")),
        gtkw: Some(PathBuf::from(format!("/runs/r/{name}.gtkw"))),
        model: 0,
        branch,
        marker: Some(SimTime(1250)),
        failing_branch: false,
        died: None,
    };
    report.waves = Some(WavesReplay {
        files: vec![
            readable("waves", vec![]),
            readable("waves.branch-0", vec![0]),
            WavesFile {
                fst: PathBuf::from("/runs/r/waves.branch-1.fst"),
                gtkw: None,
                model: 0,
                branch: vec![1],
                marker: None,
                failing_branch: true,
                died: Some(fork::ChildExit::Signaled(6)),
            },
        ],
        from: SimTime(250),
        to: None,
        precision: Precision::NS,
        verdict: Verdict::Reproduced,
    });
    let text = report.to_string();
    assert!(!text.contains("failed at"), "{text}");
    insta::assert_snapshot!(text);
}
