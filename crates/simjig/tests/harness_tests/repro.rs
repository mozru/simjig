//! `SIMJIG_SEED` reproduces a seeded failure: the repro line printed by a failing run,
//! executed as a separate process, fails the same way.

use std::path::PathBuf;
use std::process::{Command, Output};

use simjig::{Seed, SeedSource, TestCtx, rising};

use crate::counter;

/// The first draw from `"stim"`, which decides whether [`seeded_failure`] fails.
fn first_draw(seed: u64) -> u64 {
    Seed::new(seed, SeedSource::Env).rng("stim").below(1000)
}

#[simjig::test]
#[ignore = "fails for odd draws on purpose; run by `simjig_seed_reproduces_a_seeded_failure`"]
async fn seeded_failure(sim: &TestCtx, dut: counter::Dut) {
    let value = sim.rng("stim").below(1000);
    dut.en.write_u64(1);
    dut.data.write_u64(value & 0xff);
    rising(dut.clk).await;
    rising(dut.clk).await;
    assert!(value.is_multiple_of(2), "drew the odd value {value}");
}

/// Runs [`seeded_failure`] in a child process of this test binary.
fn run_child(seed: &str, runs: &str) -> Output {
    let exe = std::env::current_exe().expect("the test binary");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("harness")
        .join(runs);
    Command::new(exe)
        .args([
            "--ignored",
            "--exact",
            "repro::seeded_failure",
            "--nocapture",
        ])
        .env("SIMJIG_SEED", seed)
        .env("SIMJIG_RUNS_DIR", dir)
        .output()
        .expect("the child runs")
}

/// The report's header line (`FAIL ... [Kind sig=...]`) and its repro seed.
fn header_and_seed(output: &Output) -> (String, String) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let header = stderr
        .lines()
        .find(|l| l.starts_with("FAIL repro::seeded_failure"))
        .unwrap_or_else(|| panic!("no report in:\n{stderr}"))
        .to_owned();
    let repro = stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("repro:"))
        .unwrap_or_else(|| panic!("no repro line in:\n{stderr}"))
        .trim();
    let expected_tail =
        " cargo nextest run -p simjig --run-ignored all -E 'test(=repro::seeded_failure)'";
    let seed = repro
        .strip_prefix("SIMJIG_SEED=")
        .and_then(|r| r.strip_suffix(expected_tail))
        .unwrap_or_else(|| panic!("unexpected repro line: {repro}"))
        .to_owned();
    (header, seed)
}

#[test]
fn simjig_seed_reproduces_a_seeded_failure() {
    let odd: Vec<u64> = (0..1000)
        .filter(|&s| !first_draw(s).is_multiple_of(2))
        .take(2)
        .collect();
    let even = (0..1000)
        .find(|&s| first_draw(s).is_multiple_of(2))
        .expect("an even draw");

    let first = run_child(&odd[0].to_string(), "repro-1");
    assert!(!first.status.success(), "seed {} must fail", odd[0]);
    let (header, seed) = header_and_seed(&first);
    assert_eq!(seed, Seed::new(odd[0], SeedSource::Env).to_string());
    assert!(header.contains("[TaskPanic sig="), "{header}");

    // The printed repro seed replays the same failure at the same time.
    let again = run_child(&seed, "repro-2");
    assert_eq!(header_and_seed(&again).0, header);

    // Another failing seed lands in the same bucket (numbers are masked)...
    let other = run_child(&format!("{:#x}", odd[1]), "repro-3");
    let sig = |h: &str| h.rsplit_once("sig=").map(|(_, s)| s.to_owned());
    assert_eq!(sig(&header_and_seed(&other).0), sig(&header));
    // ...and a passing seed passes.
    assert!(run_child(&even.to_string(), "repro-4").status.success());
}
