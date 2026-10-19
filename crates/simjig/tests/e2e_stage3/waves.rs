//! (e) Waves on failure: a failing `#[simjig::test]` of this binary runs in a child
//! process with `SIMJIG_WAVES_ON_FAIL=1`; the harness replays it with tracing, and its run
//! directory gets `waves.fst` ending at the failure, plus the `.gtkw` view.

use std::path::PathBuf;
use std::process::Command;

use simjig::{clock, ns, read_only};

use crate::counter_fst::Dut;
use crate::fst;

/// Fails at the 40th rising edge after reset (t = 400 ns).
#[simjig::test(timeout = "10us")]
#[ignore = "fails on purpose; run by the test below in a child process"]
async fn the_counter_misses_its_target(dut: Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.en.set(1);
    dut.clk.falling().await;
    dut.rst.set(0);
    for _ in 0..40 {
        dut.clk.rising().await;
    }
    read_only().await;
    assert_eq!(dut.count.get().to_u128(), 41, "off by one");
}

#[test]
fn a_failing_test_gets_waves_from_its_replay() {
    let test = "waves::the_counter_misses_its_target";
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e-stage3-waves");
    std::fs::remove_dir_all(&root).ok();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", test, "--nocapture"])
        .env("SIMJIG_RUNS_DIR", &root)
        .env("SIMJIG_WAVES_ON_FAIL", "1");
    for var in [
        "SIMJIG_SEED",
        "SIMJIG_WAVES",
        "SIMJIG_TRACE",
        "SIMJIG_TRACE_FROM",
        "SIMJIG_TRACE_TO",
    ] {
        command.env_remove(var);
    }
    let output = command.output().unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{err}");

    let tests = root
        .join("simjig")
        .join("e2e_stage3")
        .join(test.replace("::", "__"));
    let dir = std::fs::read_dir(&tests)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.'))
        .unwrap_or_else(|| panic!("no run directory under {}:\n{err}", tests.display()));
    let fst_path = dir.join("waves.fst");
    assert!(fst_path.is_file(), "{err}");
    assert!(
        err.contains(&format!("waves:    {}", fst_path.display())),
        "the report names the waves:\n{err}"
    );
    assert!(err.contains("replay:   reproduced"), "{err}");
    let waves = fst::read(&fst_path);
    // 40 rising edges after the falling edge at 5 ns: 10, 20, ..., 400 ns, in ps.
    assert_eq!(waves.end, 400_000, "the waves end at the failure");
    assert!(waves.start < waves.end && waves.times.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(waves.timescale, -12);
    assert!(waves.vars > 0 && waves.blocks > 0);
    assert!(dir.join("waves.gtkw").is_file());
}
