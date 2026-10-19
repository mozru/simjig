//! `#[simjig::test]` end to end: the expansion runs the body against the DUT, applies the
//! timeout, and fails the test with the report.

use simjig::{Failure, FailureKind, Precision, TestCtx, now, ns, read_only, rising, timer};

use crate::counter;

#[simjig::test]
async fn runs_a_body_without_parameters() {
    timer(ns(5)).await;
    assert_eq!(
        now().ticks(),
        5000,
        "no DUT: an empty model at 1 ps precision"
    );
}

#[simjig::test(timeout = "1us")]
async fn drives_the_dut(dut: counter::Dut) -> simjig::Result {
    dut.en.write_u64(1);
    dut.data.write_u64(3);
    for _ in 0..4 {
        rising(dut.clk).await;
    }
    read_only().await;
    let acc = dut.acc.read_u64();
    assert!(acc >= 9 && acc.is_multiple_of(3), "acc = {acc}");
    Ok(())
}

#[simjig::test]
async fn context_gives_the_precision_to_print_times(ctx: &TestCtx) {
    timer(ns(1250)).await;
    assert_eq!(ctx.precision(), Precision::PS, "no DUT: 1 ps");
    assert_eq!(now().display(ctx.precision()).to_string(), "1250 ns");
}

#[simjig::test]
async fn context_names_the_test_and_its_seeds(ctx: &TestCtx, _dut: counter::Dut) {
    assert_eq!(
        ctx.test_name(),
        "macros::context_names_the_test_and_its_seeds"
    );
    assert_eq!(ctx.rng("a"), ctx.seed().rng("a"));
    assert_eq!(ctx.verilator_seed(), ctx.seed().verilator_seed());
}

#[simjig::test(timeout = "100ns")]
#[should_panic(expected = "[TimeLimit sig=")]
async fn timeout_fails_the_test(dut: counter::Dut) {
    loop {
        rising(dut.clk).await;
    }
}

#[simjig::test]
#[should_panic(expected = "message:  the scoreboard saw 2 extra items")]
async fn a_returned_failure_fails_the_test() -> simjig::Result {
    timer(ns(1)).await;
    let message = "the scoreboard saw 2 extra items".to_owned();
    Err(Failure {
        kind: FailureKind::ModelFatal {
            message: message.clone(),
        },
        time: now(),
        precision: Precision::PS,
        task: Some("main".into()),
        location: None,
        message,
    })
}

#[simjig::test]
#[should_panic(expected = "harness_tests/macros.rs:")]
async fn a_panic_reports_its_location() {
    panic!("boom");
}

#[simjig::test(timeout = "none")]
async fn timeout_none_runs_past_the_default() {
    timer(simjig::ms(20)).await;
}

// The body ends after 20 ms: if the 10 ms default did not fire, the test fails because it
// did not panic, rather than hanging until nextest kills it.
#[simjig::test]
#[should_panic(expected = "(the default for #[simjig::test]")]
async fn the_default_timeout_fails_a_long_test() {
    for _ in 0..20 {
        timer(simjig::ms(1)).await;
    }
}

/// Runs one test of this binary in a child process, output uncaptured.
fn run_uncaptured(test: &str) -> std::process::Output {
    std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", test, "--nocapture"])
        .env_remove("SIMJIG_SEED")
        .output()
        .expect("the child runs")
}

#[test]
fn a_passing_should_panic_test_prints_no_report() {
    let output = run_uncaptured("macros::timeout_fails_the_test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("FAIL macros::"), "{stderr}");
}

// `::simjig` resolves here too, so this checks that the expansion compiles through `::sj`;
// the macro's snapshot (`expands_with_a_renamed_crate`) checks it uses no other path.
#[simjig::test(crate = ::sj, timeout = "1us")]
async fn a_renamed_crate_is_reached_through_its_path() -> sj::Result {
    sj::yield_now().await;
    Ok(())
}
