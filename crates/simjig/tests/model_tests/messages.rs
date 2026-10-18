//! Model messages: `$fatal`, `$stop` and `$error` become `ModelFatal` failures naming the HDL
//! file, line and scope; `$finish` ends the run as `DesignFinished`; the process survives
//! every one of them; `$display` output reaches the test's own (captured) output.

use std::process::Command;

use simjig::{Failure, FailureKind, ReadOnly, Sim, SimTime};
use simjig_bits::Bits;

use super::duts::{fatal, hello, sim};
use super::start_clock;

/// Runs the `fatal` DUT: reset for one cycle, then fire `kind` on the next rising edge.
fn fire(kind: u8) -> (Result<u8, Failure>, Sim) {
    let (mut sim, dut) = sim::<fatal::Dut>();
    start_clock(&mut sim, dut.clk.raw());
    let result = sim.run(async move {
        dut.clk.set(0);
        dut.rst.set(1);
        dut.clk.rising().await;
        dut.clk.falling().await;
        dut.rst.set(0);
        dut.fire.set(1);
        dut.kind.set(u128::from(kind));
        for _ in 0..3 {
            dut.clk.rising().await;
        }
        ReadOnly::new().await;
        u8::try_from(dut.alive.get().to_u128()).unwrap()
    });
    (result, sim)
}

fn model_failure(kind: u8) -> (String, SimTime) {
    let (result, _sim) = fire(kind);
    let Err(failure) = result else {
        panic!("the design must fail the run");
    };
    let FailureKind::ModelFatal { message } = &failure.kind else {
        panic!("expected ModelFatal, got {failure}");
    };
    (message.clone(), failure.time)
}

#[test]
fn fatal_is_a_failure_with_file_line_and_scope() {
    let (message, time) = model_failure(0);
    // The second rising edge, at 15 ns, is the first one with `fire` set.
    assert_eq!(time, SimTime(15_000));
    assert!(message.starts_with("$fatal in TOP.fatal at /"), "{message}");
    assert!(
        message.ends_with("/tests/duts/fatal/fatal.sv:31: fatal: kind=0 $fatal"),
        "{message}"
    );
}

#[test]
fn stop_and_error_are_failures() {
    let (stop, _) = model_failure(1);
    assert!(stop.starts_with("$stop at "), "{stop}");
    assert!(stop.contains("fatal.sv:32"), "{stop}");
    let (error, _) = model_failure(2);
    assert!(error.starts_with("$error in TOP.fatal at /"), "{error}");
    assert!(
        error.ends_with("fatal.sv:33: fatal: kind=2 $error"),
        "{error}"
    );
}

#[test]
fn finish_ends_the_run() {
    let (result, sim) = fire(3);
    let failure = result.expect_err("$finish before main returns fails the run");
    assert_eq!(failure.kind, FailureKind::DesignFinished);
    assert_eq!(sim.now(), SimTime(15_000));
}

#[test]
fn the_process_survives_a_fatal_and_runs_another_model() {
    let (first, _) = model_failure(0);
    assert!(first.contains("$fatal"), "{first}");
    // A fresh model in the same process: unrelated to the poisoned one, and healthy.
    let (mut sim, dut) = sim::<fatal::Dut>();
    start_clock(&mut sim, dut.clk.raw());
    let alive = sim.run(async move {
        dut.rst.set(0);
        dut.fire.set(0);
        for _ in 0..4 {
            dut.clk.rising().await;
        }
        ReadOnly::new().await;
        dut.alive.get()
    });
    assert_eq!(alive.unwrap(), Bits::new(4));
}

/// Run by [`display_reaches_the_test_output`] in a child process.
#[test]
#[ignore = "child of display_reaches_the_test_output"]
fn hello_child() {
    let (mut sim, dut) = sim::<hello::Dut>();
    start_clock(&mut sim, dut.clk.raw());
    let n = sim.run(async move {
        for _ in 0..3 {
            dut.clk.rising().await;
        }
        ReadOnly::new().await;
        dut.n.get()
    });
    assert_eq!(n.unwrap(), Bits::new(3));
}

fn run_child(capture: bool) -> String {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.args([
        "--exact",
        "messages::hello_child",
        "--ignored",
        "--test-threads",
        "1",
    ]);
    if !capture {
        cmd.arg("--nocapture");
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "child failed:\n{stdout}");
    stdout
}

#[test]
fn display_reaches_the_test_output() {
    let shown = run_child(false);
    for edge in 0..3 {
        let line = format!("hello: edge {edge} in TOP.hello\n");
        assert!(shown.contains(&line), "missing {line:?} in:\n{shown}");
    }
    assert!(shown.contains("hello: second edge"), "{shown}");
    // With capture on, the harness swallows the output of a passing test: the text went
    // through the test's own stdout, not straight to file descriptor 1.
    let captured = run_child(true);
    assert!(!captured.contains("hello: edge"), "{captured}");
}
