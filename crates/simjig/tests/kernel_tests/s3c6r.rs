//! Stage 3 cycle 6 adversarial review: probes of the main-return exemption of the
//! dropped-unrun-`Task` rule, and of `Failure::from(Elapsed)`.

use std::cell::Cell;
use std::rc::Rc;

use simjig::{Event, FailureKind, Timer, first, scope, spawn, with_timeout};

use super::{dut, ticks};

/// Probe: `let _ = spawn(..)` as main's last statement. Documented to pass (a detached
/// task would not run either); recorded here so the behaviour is pinned.
#[test]
fn s3c6r_let_underscore_as_mains_last_statement_passes() {
    let mut d = dut();
    let ran = Rc::new(Cell::new(false));
    let flag = Rc::clone(&ran);
    let out = d.sim.run(async move {
        Timer::new(ticks(1)).await;
        #[expect(clippy::let_underscore_future, reason = "the mistake under test")]
        let _ = spawn("check", async move { flag.set(true) });
    });
    assert!(out.is_ok());
    assert!(!ran.get());
}

/// `with_timeout` around main's body: the drop is in the first poll, main pends; must fail.
#[test]
fn s3c6r_let_underscore_inside_with_timeout_around_main_fails() {
    let mut d = dut();
    let f = d
        .sim
        .run(async move {
            with_timeout(ticks(100), async {
                #[expect(clippy::let_underscore_future, reason = "the mistake under test")]
                let _ = spawn("drv", async {});
                Timer::new(ticks(5)).await;
            })
            .await
            .unwrap();
        })
        .unwrap_err();
    assert!(matches!(f.kind, FailureKind::Setup { .. }), "{f}");
}

/// A first! operand that drops an unrun handle in the poll where the other operand wins
/// and main returns: nothing could have run it.
#[test]
fn s3c6r_drop_in_a_first_operand_in_mains_last_poll() {
    let mut d = dut();
    let out = d.sim.run(async move {
        let never = Event::new("never");
        first!(async {}, async {
            #[expect(clippy::let_underscore_future, reason = "the mistake under test")]
            let _ = spawn("drv", async {});
            never.wait().await;
        });
        1
    });
    println!("first-operand: {out:?}");
}

/// Nested scope whose last poll drops an unrun handle, then main returns: the scope is a
/// task, so it fails even though main returns right after.
#[test]
fn s3c6r_nested_scope_drop_fails() {
    let mut d = dut();
    let f = d
        .sim
        .run(async move {
            scope("outer", async {
                scope("inner", async {
                    #[expect(clippy::let_underscore_future, reason = "the mistake under test")]
                    let _ = spawn("drv", async {});
                })
                .await;
            })
            .await;
        })
        .unwrap_err();
    assert!(matches!(f.kind, FailureKind::Setup { .. }), "{f}");
    assert_eq!(f.task.as_deref(), Some("main.outer.inner"));
}

/// A detached child drops an unrun handle in its last poll: the task is freed before the
/// poll is settled, and the failure still names the child (not `<finished task>`).
#[test]
fn s3c6r_a_detached_child_unrun_drop_names_the_child() {
    let mut d = dut();
    let f = d
        .sim
        .run(async move {
            spawn("child", async move {
                #[expect(clippy::let_underscore_future, reason = "the mistake under test")]
                let _ = spawn("lost", async {});
            })
            .detach();
            Timer::new(ticks(1)).await;
        })
        .unwrap_err();
    assert!(matches!(f.kind, FailureKind::Setup { .. }), "{f}");
    assert_eq!(f.task.as_deref(), Some("main.child"), "{f}");
}
