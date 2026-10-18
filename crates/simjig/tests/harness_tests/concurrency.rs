//! Run directories under concurrency: several processes running the same test with the
//! same seed (two `cargo nextest` invocations, or a stress loop) must each write a
//! complete run directory, and readers must never see a half-written one.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use simjig::rising;

use crate::counter;

#[simjig::test]
#[ignore = "a writer for `concurrent_runs_of_one_test_all_write_their_run_dir`"]
async fn writer(dut: counter::Dut) {
    dut.en.write_u64(1);
    for i in 0..300 {
        dut.data.write_u64(i & 0xff);
        rising(dut.clk).await;
    }
}

fn spawn_writer(runs: &Path) -> Child {
    Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--ignored", "--exact", "concurrency::writer", "--nocapture"])
        .env("SIMJIG_SEED", "7")
        .env("SIMJIG_RUNS_DIR", runs)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the child starts")
}

#[test]
fn concurrent_runs_of_one_test_all_write_their_run_dir() {
    const PROCESSES: usize = 6;
    const ROUNDS: usize = 8;
    let runs = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("harness")
        .join("concurrency");
    // Absent on a first run; a leftover from an earlier one would hide this run's result.
    std::fs::remove_dir_all(&runs).ok();
    let mut failures = Vec::new();
    for _ in 0..ROUNDS {
        let children: Vec<Child> = (0..PROCESSES).map(|_| spawn_writer(&runs)).collect();
        for child in children {
            let output = child.wait_with_output().expect("the child exits");
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if !output.status.success() || stderr.contains("run directory not written") {
                failures.push(stderr);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} runs failed; the first:\n{}",
        failures.len(),
        PROCESSES * ROUNDS,
        failures[0]
    );
    let dirs: Vec<PathBuf> = walk(&runs)
        .into_iter()
        .filter(|d| d.join("summary.json").is_file())
        .collect();
    assert_eq!(dirs.len(), 1, "one test and seed, one run dir: {dirs:?}");
    let summary = std::fs::read_to_string(dirs[0].join("summary.json")).expect("readable");
    let summary: serde_json::Value = serde_json::from_str(&summary).expect("complete JSON");
    assert_eq!(summary["status"], "pass");
    let leftovers: Vec<PathBuf> = walk(&runs)
        .into_iter()
        .filter(|d| {
            d.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary directories left: {leftovers:?}"
    );
}

/// Every directory under `root`, recursively.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
                out.push(path);
            }
        }
    }
    out
}
