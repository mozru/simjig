//! Synchronous tests (`Sim::run_sync`, `SyncClock`) against real Verilator models: writes
//! taken by the next eval, two evals per cycle, post-edge reads, model failures and the time
//! limit ending the body, and the async features refused by name.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use simjig::{FailureKind, SimDuration, SimTime, SyncClock, spawn};

use super::duts::{counter, fatal, sim};
use super::ticks;

fn clock(clk: impl Into<simjig::RawSig>) -> SyncClock {
    SyncClock::new(clk, ticks(10))
}

/// A write is in the model at once but changes nothing until an eval; after `tick()` the
/// flop has taken it and reads show the settled post-edge value. Each cycle is two evals.
#[test]
fn a_tick_is_two_evals_and_reads_see_the_settled_edge() {
    let (mut sim, dut) = sim::<counter::Dut>();
    let counts = sim
        .run_sync(|| {
            let mut cyc = clock(dut.clk.raw());
            dut.rst.set(1);
            cyc.tick();
            dut.rst.set(0);
            dut.en.set(1);
            let before = dut.count.get().to_u128();
            cyc.tick();
            let one = dut.count.get().to_u128();
            cyc.ticks(9);
            (before, one, dut.count.get().to_u128(), cyc.now())
        })
        .unwrap();
    assert_eq!(counts, (0, 1, 10, SimTime(110)));
    let stats = sim.stats();
    // The first eval at t=0, then two per cycle.
    assert_eq!(stats.evals, 1 + 2 * 11);
    // Nothing was hashed: the trace hash is still FNV-1a's offset basis.
    assert_eq!(sim.trace_hash(), 0xcbf2_9ce4_8422_2325);
}

/// `rise()` and `fall()` are the two halves of `tick()`: the counter moves on the rise.
#[test]
fn half_cycles_step_one_edge_each() {
    let (mut sim, dut) = sim::<counter::Dut>();
    let seen = sim
        .run_sync(|| {
            let mut cyc = clock(dut.clk.raw());
            dut.rst.set(1);
            cyc.tick();
            dut.rst.set(0);
            dut.en.set(1);
            cyc.rise();
            let after_rise = dut.count.get().to_u128();
            cyc.fall();
            (after_rise, dut.count.get().to_u128())
        })
        .unwrap();
    assert_eq!(seen, (1, 1));
}

/// A `$fatal` fails the run at the eval that ran it; the body stops there.
#[test]
fn a_model_failure_ends_the_body() {
    let (mut sim, dut) = sim::<fatal::Dut>();
    let mut reached = false;
    let failure = sim
        .run_sync(|| {
            let mut cyc = clock(dut.clk.raw());
            dut.rst.set(1);
            cyc.tick();
            dut.rst.set(0);
            dut.fire.set(1);
            dut.kind.set(0u128);
            cyc.tick();
            reached = true;
        })
        .unwrap_err();
    assert!(!reached, "the body ran past the failing eval");
    assert!(
        matches!(&failure.kind, FailureKind::ModelFatal { message } if message.contains("$fatal")),
        "{failure}"
    );
    assert_eq!(failure.time, SimTime(15));
}

/// The time limit is checked before each half cycle.
#[test]
fn the_time_limit_ends_the_body() {
    let (mut sim, dut) = sim::<counter::Dut>();
    sim.set_time_limit(SimTime(100));
    let failure = sim
        .run_sync(|| {
            let mut cyc = clock(dut.clk.raw());
            loop {
                cyc.tick();
            }
        })
        .unwrap_err();
    assert!(
        matches!(failure.kind, FailureKind::TimeLimit { .. }),
        "{failure}"
    );
    assert_eq!(failure.time, SimTime(100));
    // No tasks to blame in a synchronous test.
    let help = failure.help().unwrap();
    assert!(!help.contains("task"), "{help}");
}

/// A panic in the body is a `TaskPanic` of `main`, at the panicking line.
#[test]
fn a_panic_in_the_body_is_a_task_panic_of_main() {
    let (mut sim, _dut) = sim::<counter::Dut>();
    let line = line!() + 1;
    let failure = sim.run_sync(|| panic!("checked")).unwrap_err();
    assert_eq!(
        failure.kind,
        FailureKind::TaskPanic {
            message: "checked".into()
        }
    );
    assert_eq!(failure.task.as_deref(), Some("main"));
    let at = failure.location.expect("the hook saw the panic");
    assert_eq!((at.file(), at.line()), (file!(), line));
}

fn message(failure: &simjig::Failure) -> String {
    match &failure.kind {
        FailureKind::TaskPanic { message } => message.clone(),
        other => panic!("expected a TaskPanic, got {other:?}"),
    }
}

/// A panic in the body: its message and line (a `TaskPanic` of `main`).
fn panicked(body: impl FnOnce(counter::Dut)) -> (String, Option<u32>) {
    let (mut sim, dut) = sim::<counter::Dut>();
    let failure = sim.run_sync(|| body(dut)).unwrap_err();
    (message(&failure), failure.location.map(simjig::SourceLocation::line))
}

/// `spawn`, a kernel clock and an awaited trigger name the mode instead of running.
#[test]
fn async_features_are_refused_by_name() {
    let refused = |what: &str, body: &dyn Fn(counter::Dut)| {
        let (mut sim, dut) = sim::<counter::Dut>();
        let failure = sim.run_sync(|| body(dut)).unwrap_err();
        let text = message(&failure);
        assert!(
            text.contains("synchronous test (`#[simjig::test(sync)]`)"),
            "{what}: {text}"
        );
    };
    refused("spawn", &|_| drop(spawn("t", async {})));
    refused("clock", &|dut| {
        simjig::clock(dut.clk.raw(), SimDuration::from_ticks(10)).start();
    });
    // At the line that starts it.
    let line = line!() + 2;
    let (_, at) = panicked(|dut| {
        simjig::clock(dut.clk.raw(), SimDuration::from_ticks(10)).start();
    });
    assert_eq!(at, Some(line));
    refused("trigger", &|dut| {
        let mut edge = pin!(dut.clk.rising());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(edge.as_mut().poll(&mut cx), Poll::Pending));
    });
}

/// A bound handle reads and writes what the handle does, without its lookups.
#[test]
fn bound_handles_read_and_write_what_handles_do() {
    let (mut sim, dut) = sim::<counter::Dut>();
    let seen = sim
        .run_sync(|| {
            let mut cyc = clock(dut.clk.raw());
            let (rst, en, count) = (cyc.bind(dut.rst), cyc.bind(dut.en), cyc.bind(dut.count));
            rst.set(1);
            cyc.tick();
            rst.set(0);
            en.set(1);
            cyc.ticks(5);
            (
                count.get().to_u128(),
                dut.count.get().to_u128(),
                en.get(),
                rst.get(),
            )
        })
        .unwrap();
    assert_eq!(
        seen,
        (5, 5, simjig::bits::Bit::ONE, simjig::bits::Bit::ZERO)
    );
}

/// With two models nothing is held, so bound handles take the handles' own path; the
/// results are the same, and both models step with the clock of the first.
#[test]
fn bound_handles_of_two_models_take_the_handle_path() {
    let (mut sim, (a, b)) = sim::<(counter::Dut, counter::Dut)>();
    let seen = sim
        .run_sync(|| {
            let mut cyc = clock(a.clk.raw());
            let (rst, en, count) = (cyc.bind(a.rst), cyc.bind(a.en), cyc.bind(a.count));
            rst.set(1);
            b.rst.set(1);
            cyc.tick();
            rst.set(0);
            en.set(1);
            cyc.ticks(3);
            (count.get().to_u128(), b.count.get().to_u128())
        })
        .unwrap();
    // `b`'s clock is never driven: it stays in reset at 0.
    assert_eq!(seen, (3, 0));
}

/// The README's synchronous test ("Synchronous testbenches"), as written there but for the
/// unused context.
#[simjig::test(sync, timeout = "1ms")]
fn readme_counts(dut: counter::Dut) {
    let mut cyc = SyncClock::new(dut.clk, simjig::ns(10));
    dut.rst.set(1);
    cyc.tick(); // rise, eval, fall, eval
    dut.rst.set(0);
    dut.en.set(1);
    let count = cyc.bind(dut.count); // a bound handle: a direct read, no lookup
    for i in 1..=1000u128 {
        cyc.tick();
        assert_eq!(count.get().to_u128(), i % 256);
    }
}
