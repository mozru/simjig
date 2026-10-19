//! The rules `run_from_checkpoint` enforces before it forks, and fork hooks.

use std::cell::Cell;
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};

use simjig::fork::{self, BranchError, ForkError, ForkHook};
use simjig::{Seed, SeedSource, TestCtx, clock, ns, spawn, timer};

use crate::counter;

fn seed() -> Seed {
    Seed::new(3, SeedSource::Env)
}

/// Polls `future` once, outside any simulation.
fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
    let mut future = pin!(future);
    future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
}

#[test]
fn outside_a_simulation_nothing_forks() {
    let result = poll_once(fork::run_from_checkpoint(seed(), 2, |_| async { Ok(()) }));
    let Poll::Ready(Err(error)) = result else {
        panic!("expected an error at once");
    };
    assert_eq!(error, ForkError::NoSimulation);
}

/// A third thread (one of the test's own) could hold a lock the child needs.
#[simjig::test(timeout = "1us")]
async fn a_third_thread_refuses_the_fork(_dut: counter::Dut) {
    let (release, wait) = mpsc::channel::<()>();
    let (started, ready) = mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        started.send(()).unwrap();
        wait.recv().unwrap();
    });
    ready.recv().unwrap();
    let error = fork::run_from_checkpoint(seed(), 2, |_| async { Ok(()) })
        .await
        .unwrap_err();
    release.send(()).unwrap();
    thread.join().unwrap();
    assert_eq!(error, ForkError::Threads { count: 3 });
    let text = error.to_string();
    assert!(text.contains("has 3 threads"), "{text}");
    assert!(text.contains("cargo nextest run"), "{text}");
}

#[simjig::test(timeout = "1us")]
async fn another_running_task_refuses_the_fork(dut: counter::Dut) {
    clock(dut.clk, ns(10)).start();
    let driver = spawn("driver", async move {
        loop {
            dut.clk.rising().await;
        }
    });
    let error = fork::run_from_checkpoint(seed(), 2, |_| async { Ok(()) })
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ForkError::OtherTasks {
            tasks: vec!["main.driver".into()]
        }
    );
    driver.cancel();
    // A finished task does not count.
    let done = spawn("done", async {});
    timer(ns(1)).await;
    assert!(done.is_finished());
    let branches = fork::run_from_checkpoint(seed(), 1, |_| async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(branches.into_values(), [()]);
}

#[simjig::test(timeout = "1us")]
async fn a_task_other_than_main_cannot_fork(_dut: counter::Dut) {
    let error = spawn("helper", async {
        fork::run_from_checkpoint(seed(), 1, |_| async { Ok(()) })
            .await
            .unwrap_err()
    })
    .await;
    assert_eq!(
        error,
        ForkError::NotMainTask {
            task: "main.helper".into()
        }
    );
}

thread_local! {
    /// What the child hook saw, read back by the branch body in the same (child) process.
    static CHILD_HOOK_SAW: Cell<Option<usize>> = const { Cell::new(None) };
}

#[simjig::test(timeout = "1us")]
async fn hooks_run_before_each_fork_and_in_each_child(ctx: &TestCtx, _dut: counter::Dut) {
    let before = Rc::new(Cell::new(0));
    let counter = Rc::clone(&before);
    let _guard = fork::register_hook(ForkHook::new(
        move || {
            counter.set(counter.get() + 1);
            Ok(())
        },
        |branch| {
            CHILD_HOOK_SAW.set(Some(branch.index()));
            if branch.index() == 2 {
                Err("cannot reopen the trace".into())
            } else {
                Ok(())
            }
        },
    ));
    let branches = fork::run_from_checkpoint(ctx.seed(), 3, |_| async { Ok(CHILD_HOOK_SAW.get()) })
        .await
        .unwrap();
    assert_eq!(before.get(), 3, "once per fork, in the parent");
    assert_eq!(
        CHILD_HOOK_SAW.get(),
        None,
        "the child hook never runs in the parent"
    );
    let outcomes = branches.outcomes();
    assert_eq!(outcomes[0].result, Ok(Some(0)));
    assert_eq!(outcomes[1].result, Ok(Some(1)));
    let Err(BranchError::Failed(failure)) = &outcomes[2].result else {
        panic!("{:?}", outcomes[2].result);
    };
    assert_eq!(
        failure.message,
        "a fork hook failed in the child: cannot reopen the trace"
    );
}

#[simjig::test(timeout = "1us")]
async fn a_failing_hook_before_the_fork_cancels_the_checkpoint(_dut: counter::Dut) {
    let forked = Rc::new(Cell::new(false));
    let _guard = fork::register_hook(ForkHook::new(|| Err("cannot flush".into()), |_| Ok(())));
    let flag = Rc::clone(&forked);
    let error = fork::run_from_checkpoint(seed(), 2, move |_| {
        flag.set(true);
        async { Ok(()) }
    })
    .await
    .unwrap_err();
    assert_eq!(
        error,
        ForkError::Hook {
            message: "cannot flush".into()
        }
    );
    assert!(!forked.get());
}

#[simjig::test(timeout = "1us")]
async fn a_dropped_hook_guard_removes_the_hook(_dut: counter::Dut) {
    let guard = fork::register_hook(ForkHook::new(|| Err("stale".into()), |_| Ok(())));
    drop(guard);
    let branches = fork::run_from_checkpoint(seed(), 1, |_| async { Ok(1u8) })
        .await
        .unwrap();
    assert_eq!(branches.into_values(), [1]);
}

/// Prints a line without its newline (so it sits in stdout's buffer), then forks. Never
/// passes on its own; run by `buffered_output_is_printed_once`.
#[simjig::test(timeout = "1us")]
#[ignore = "child of buffered_output_is_printed_once"]
async fn buffered_output_child(_dut: counter::Dut) {
    print!("{MARK}");
    let branches = fork::run_from_checkpoint(seed(), 2, |_| async { Ok(()) })
        .await
        .unwrap();
    println!();
    branches.into_values();
}

const MARK: &str = "simjig-fork-mark";

/// Output buffered at the fork is flushed first, so the branches do not print it again.
#[test]
fn buffered_output_is_printed_once() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "rules::buffered_output_child",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.matches(MARK).count(), 1, "{stdout}");
}

/// A checkpoint inside a fork branch is refused with its own error, not as a quiet-point
/// problem (the branch's kernel-internal task) the user cannot fix.
#[simjig::test(timeout = "1us")]
async fn a_checkpoint_inside_a_branch_is_refused_as_nested(ctx: &TestCtx, dut: counter::Dut) {
    clock(dut.clk, ns(10)).start();
    let branches = fork::run_from_checkpoint(ctx.seed(), 2, |branch| async move {
        dut.clk.rising().await;
        let inner = fork::run_from_checkpoint(branch.seed(), 2, |_| async { Ok(()) }).await;
        Ok(format!("{:?}", inner.map(|_| ())))
    })
    .await
    .unwrap();
    let expected = format!("{:?}", Err::<(), _>(ForkError::Nested));
    assert_eq!(branches.into_values(), [expected.clone(), expected]);
    assert_eq!(
        ForkError::Nested.to_string(),
        "checkpoints cannot be nested inside a fork branch"
    );
}
