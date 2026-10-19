//! The dropped-unrun-`Task` check against early returns. A task left unrun because its
//! spawner returned an error in the same delta is a consequence of that error, not `let _ =
//! spawn(..)`; the returned error must reach the caller.

use simjig::fork::{self, ForkError};
use simjig::{Failure, FailureKind, Precision, now, spawn, timer};

use super::{assert_clean, dut, ticks};

fn scoreboard_error() -> Failure {
    let message = "the scoreboard saw 2 extra items".to_owned();
    Failure {
        kind: FailureKind::ModelFatal {
            message: message.clone(),
        },
        time: now(),
        precision: Precision::NS,
        task: Some("main".into()),
        location: None,
        message,
    }
}

/// A synchronous check after spawning a monitor fails with `?`: the run must report the
/// scoreboard's error, not a `Setup` failure blaming the monitor's `spawn`.
#[test]
#[expect(
    clippy::result_large_err,
    reason = "simjig::Result is the type tests return"
)]
fn an_early_error_return_is_not_masked_by_the_unrun_monitor() {
    let mut d = dut();
    let check = || -> simjig::Result { Err(scoreboard_error()) };
    let result = d.sim.run(async move {
        let monitor = spawn("monitor", timer(ticks(100)));
        check()?;
        monitor.await;
        simjig::Result::Ok(())
    });
    let returned = result.unwrap_or_else(|f| panic!("the run failed instead: {f}"));
    let error: Failure = returned.unwrap_err();
    assert!(error.message.contains("scoreboard"), "{error}");
    assert_clean(&d.sim);
}

/// The fork seam: a background task makes the checkpoint refuse (`OtherTasks`), and the
/// refusal propagated with `?` is replaced by a `Setup` failure about `let _ = spawn(..)`.
#[test]
fn a_fork_refusal_is_not_masked_by_the_unrun_task_it_names() {
    let mut d = dut();
    let result = d.sim.run(async move {
        let monitor = spawn("monitor", timer(ticks(100)));
        let seed = simjig::Seed::new(1, simjig::SeedSource::Env);
        fork::run_from_checkpoint(seed, 2, |_| async { Ok(()) }).await?;
        monitor.await;
        Ok::<(), ForkError>(())
    });
    let returned = result.unwrap_or_else(|f| panic!("the run failed instead: {f}"));
    let error = returned.unwrap_err();
    assert!(matches!(error, ForkError::OtherTasks { .. }), "{error:?}");
    assert_clean(&d.sim);
}
