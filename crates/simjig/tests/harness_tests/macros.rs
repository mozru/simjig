//! `#[simjig::test]` end to end: the expansion runs the body against the DUT, applies the
//! timeout, and fails the test with the report.

use simjig::{Failure, FailureKind, Precision, ReadOnly, TestCtx, Timer, now, ns, rising};

use crate::counter;

#[simjig::test]
async fn runs_a_body_without_parameters() {
    Timer::new(ns(5)).await;
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
    ReadOnly::new().await;
    let acc = dut.acc.read_u64();
    assert!(acc >= 9 && acc.is_multiple_of(3), "acc = {acc}");
    Ok(())
}

#[simjig::test]
async fn context_names_the_test_and_its_seeds(sim: &TestCtx, _dut: counter::Dut) {
    assert_eq!(
        sim.test_name(),
        "macros::context_names_the_test_and_its_seeds"
    );
    assert_eq!(sim.rng("a"), sim.seed().rng("a"));
    assert_eq!(sim.verilator_seed(), sim.seed().verilator_seed());
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
    Timer::new(ns(1)).await;
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
