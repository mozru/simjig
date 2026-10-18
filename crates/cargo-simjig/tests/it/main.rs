//! Runs the real `cargo simjig doctor` in this repository, inside its pixi environment.
//!
//! This is the one test that depends on the machine: the pinned pixi environment must
//! pass every check. When the test itself is not run under pixi, it wraps the binary in
//! `pixi run`.

use std::env;
use std::path::Path;
use std::process::{Command, Output};

/// `cargo simjig doctor <extra>` in the repository root, inside the pixi environment.
fn doctor(extra: &[&str]) -> Command {
    let binary = env!("CARGO_BIN_EXE_cargo-simjig");
    let mut command = if env::var_os("PIXI_ENVIRONMENT_NAME").is_some() {
        Command::new(binary)
    } else {
        let mut pixi = Command::new("pixi");
        pixi.args(["run", "--locked", binary]);
        pixi
    };
    command
        .args(["simjig", "doctor"])
        .args(extra)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    command
}

fn run(mut command: Command) -> (Output, String) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("cannot start doctor: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (output, stdout)
}

#[test]
fn doctor_passes_in_this_repo_under_pixi() {
    let (output, stdout) = run(doctor(&[]));
    assert!(output.status.success(), "doctor failed:\n{stdout}");
    for check in [
        "verilator",
        "verilator-root",
        "fst",
        "cxx",
        "ccache",
        "nextest",
        "paths",
        "environment",
    ] {
        let line = stdout
            .lines()
            .find(|line| line.split_whitespace().nth(1) == Some(check))
            .unwrap_or_else(|| panic!("no `{check}` line in:\n{stdout}"));
        assert!(line.starts_with("OK "), "{line}");
    }
    assert!(stdout.contains("Verilator 5.052 at "), "{stdout}");
    assert!(stdout.contains("pixi environment `default`"), "{stdout}");
    assert!(
        stdout.ends_with("doctor: 8 ok, 0 warn, 0 fail\n"),
        "{stdout}"
    );
}

#[test]
fn doctor_json_reports_ok() {
    let (output, stdout) = run(doctor(&["--json"]));
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.starts_with("{\"ok\":true,\"checks\":["), "{stdout}");
    assert_eq!(stdout.matches("\"status\":\"ok\"").count(), 8, "{stdout}");
}

#[test]
fn doctor_fails_on_inconsistent_verilator_root() {
    // Verilator itself refuses to run with this setting, so doctor must exit non-zero.
    let mut command = doctor(&[]);
    command.env("VERILATOR_ROOT", env!("CARGO_MANIFEST_DIR"));
    let (output, stdout) = run(command);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("FAIL verilator-root"), "{stdout}");
    assert!(stdout.contains("fix: unset VERILATOR_ROOT"), "{stdout}");
}

/// `cargo simjig waves` in one command: runs simjig's own passing `on_fail::counter_counts`
/// (an FST-traced counter) and finds its waves in the run directory.
///
/// No `-p`: from the workspace root, nextest then builds what `cargo xtask ci`'s
/// `nextest run --workspace` already built (same feature unification), so this test never
/// rebuilds test binaries that other tests are running.
#[test]
fn waves_writes_the_waves_of_a_test_in_one_command() {
    let runs = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-waves");
    std::fs::remove_dir_all(&runs).ok();
    let binary = env!("CARGO_BIN_EXE_cargo-simjig");
    let mut command = if env::var_os("PIXI_ENVIRONMENT_NAME").is_some() {
        Command::new(binary)
    } else {
        let mut pixi = Command::new("pixi");
        pixi.args(["run", "--locked", binary]);
        pixi
    };
    command
        .args([
            "simjig",
            "waves",
            "on_fail::counter_counts",
            "--seed",
            "0x2a",
            "--window",
            "100ns..",
        ])
        .env("SIMJIG_RUNS_DIR", &runs)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let output = command.output().expect("cargo simjig waves runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let dir = runs.join("simjig/waves_tests/on_fail__counter_counts/000000000000002a");
    assert!(dir.join("waves.fst").is_file(), "{stderr}");
    assert!(dir.join("waves.gtkw").is_file(), "{stderr}");
    assert!(stderr.contains("replay:   reproduced"), "{stderr}");
}
