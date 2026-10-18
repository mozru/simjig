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

/// The variables that tell the harness it runs inside a pixi (or conda) environment.
const LAUNCHER_VARS: [&str; 6] = [
    "PIXI_PROJECT_ROOT",
    "PIXI_PROJECT_MANIFEST",
    "PIXI_WORKSPACE_ROOT",
    "PIXI_WORKSPACE_MANIFEST",
    "PIXI_ENVIRONMENT_NAME",
    "CONDA_PREFIX",
];

/// The environment a child test process runs in.
#[derive(Clone, Copy)]
enum Shell {
    /// A plain shell: no pixi or conda variables.
    Plain,
    /// Inside `pixi run`, in a project that contains the working directory.
    Pixi,
}

/// Runs [`seeded_failure`] in a child process of this test binary, writing its run
/// directory under `$CARGO_TARGET_TMPDIR/harness/<runs>` (emptied first, so a leftover from
/// an earlier run cannot pass for this one's).
fn run_child(seed: &str, runs: &str, shell: Shell) -> Output {
    let exe = std::env::current_exe().expect("the test binary");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("harness")
        .join(runs);
    std::fs::remove_dir_all(&dir).ok(); // absent on a first run
    let mut command = Command::new(exe);
    command
        .args([
            "--ignored",
            "--exact",
            "repro::seeded_failure",
            "--nocapture",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("SIMJIG_SEED", seed)
        .env("SIMJIG_RUNS_DIR", dir);
    for var in LAUNCHER_VARS {
        command.env_remove(var);
    }
    if let Shell::Pixi = shell {
        command
            .env("PIXI_PROJECT_ROOT", env!("CARGO_MANIFEST_DIR"))
            .env("PIXI_ENVIRONMENT_NAME", "default");
    }
    command.output().expect("the child runs")
}

/// The report's header line (`FAIL ... [Kind sig=...]`), its repro seed, and what the
/// repro line runs `cargo` with (`""` or `"pixi run "`).
fn header_and_seed(output: &Output) -> (String, String, String) {
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
    let expected_tail = "cargo nextest run -p simjig --run-ignored all \
         -E 'binary(=harness_tests) & test(=repro::seeded_failure)'";
    let (seed, launcher) = repro
        .strip_prefix("SIMJIG_SEED=")
        .and_then(|r| r.strip_suffix(expected_tail))
        .and_then(|r| r.split_once(' '))
        .unwrap_or_else(|| panic!("unexpected repro line: {repro}"));
    (header, seed.to_owned(), launcher.to_owned())
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

    let first = run_child(&odd[0].to_string(), "repro-1", Shell::Plain);
    assert!(!first.status.success(), "seed {} must fail", odd[0]);
    let (header, seed, launcher) = header_and_seed(&first);
    assert_eq!(seed, Seed::new(odd[0], SeedSource::Env).to_string());
    assert!(header.contains("[TaskPanic sig="), "{header}");
    assert_eq!(launcher, "", "a plain shell runs cargo directly");

    // The printed repro seed replays the same failure at the same time.
    let again = run_child(&seed, "repro-2", Shell::Plain);
    assert_eq!(header_and_seed(&again).0, header);

    // Another failing seed lands in the same bucket (numbers are masked)...
    let other = run_child(&format!("{:#x}", odd[1]), "repro-3", Shell::Plain);
    let sig = |h: &str| h.rsplit_once("sig=").map(|(_, s)| s.to_owned());
    assert_eq!(sig(&header_and_seed(&other).0), sig(&header));
    // ...and a passing seed passes.
    assert!(
        run_child(&even.to_string(), "repro-4", Shell::Plain)
            .status
            .success()
    );
}

#[test]
fn the_repro_line_runs_cargo_through_pixi_inside_a_pixi_environment() {
    let odd = (0..1000)
        .find(|&s| !first_draw(s).is_multiple_of(2))
        .expect("an odd draw");
    let output = run_child(&odd.to_string(), "repro-pixi", Shell::Pixi);
    let (_, _, launcher) = header_and_seed(&output);
    // Outside pixi, cargo would rerun build scripts without Verilator on PATH.
    assert_eq!(launcher, "pixi run ");

    let summary = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("harness/repro-pixi")
            .join(format!(
                "simjig/harness_tests/repro__seeded_failure/{odd:016x}/summary.json"
            )),
    );
    let summary: serde_json::Value =
        serde_json::from_str(&summary.expect("written")).expect("valid JSON");
    assert_eq!(summary["launcher"], "pixi run");
}
