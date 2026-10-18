//! `fatal`: a `$fatal` in the design fails the test with the usual report, whose repro
//! line reruns the same test with the same seed. The failing test runs in a child process
//! (this binary, `--ignored --exact`), so its report can be checked.

use std::process::Command;

use simjig::{clock, ns};

use crate::fatal;

/// Fires `$fatal` on the second rising edge; never passes.
#[simjig::test(timeout = "1us")]
#[ignore = "child of a_fatal_is_a_failure_report_with_a_repro_line"]
async fn fatal_child(dut: fatal::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    dut.kind.set(0);
    dut.fire.set(1);
    for _ in 0..4 {
        dut.clk.rising().await;
    }
    unreachable!("the design's $fatal ends the run first");
}

/// Runs `fatal_child` and returns its output (libtest prints the report with the failure).
fn child(seed: Option<&str>) -> String {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "fatal_tb::fatal_child",
        "--ignored",
        "--test-threads",
        "1",
    ]);
    cmd.env_remove("SIMJIG_SEED");
    if let Some(seed) = seed {
        cmd.env("SIMJIG_SEED", seed);
    }
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "the child must fail");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// The value of `key=` in the report's first line.
fn field<'a>(report: &'a str, key: &str) -> &'a str {
    let header = report.lines().find(|l| l.starts_with("FAIL ")).unwrap();
    let start = header.find(key).unwrap() + key.len();
    header[start..].split_whitespace().next().unwrap()
}

#[test]
fn a_fatal_is_a_failure_report_with_a_repro_line() {
    let report = child(None);
    let header = report.lines().find(|l| l.starts_with("FAIL ")).unwrap();
    assert!(header.contains("fatal_tb::fatal_child"), "{report}");
    assert!(header.contains("t=10 ns"), "{report}");
    assert!(header.contains("[ModelFatal sig="), "{report}");
    assert!(
        report.contains("$fatal in TOP.fatal at ")
            && report.contains("fatal.sv:31: fatal: kind=0 $fatal"),
        "{report}"
    );
    let seed = field(&report, "seed=");
    // The repro may start with `pixi run` (the harness's own tests check when).
    let repro = report
        .lines()
        .find_map(|l| l.strip_prefix("  repro:    "))
        .unwrap_or_else(|| panic!("no repro line in:\n{report}"));
    assert!(
        repro.starts_with(&format!("SIMJIG_SEED={seed} "))
            && repro.ends_with(
                "cargo nextest run -p simjig --run-ignored all \
                 -E 'binary(=e2e_dut) & test(=fatal_tb::fatal_child)'"
            ),
        "unexpected repro line {repro:?} in:\n{report}"
    );

    // The repro's seed reproduces the same failure.
    let again = child(Some(seed));
    assert_eq!(field(&again, "seed="), seed);
    assert_eq!(field(&again, "sig="), field(&report, "sig="));
}
