//! A DPI implementation that panics, is missing, or touches a signal fails the run with a
//! report; the process does not abort, and the model is not evaluated again.

use std::cell::Cell;
use std::rc::Rc;

use simjig::{Failure, FailureKind, timer};

use super::{TICK, cycles, dpi, sim};

/// Panics on `dpi_add(7, _)`; counts its calls.
struct Picky {
    calls: Rc<Cell<u32>>,
}

impl dpi::Dpi for Picky {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        self.calls.set(self.calls.get() + 1);
        if x == 7 {
            // Marks the call that panics, so the test can tell whether any came after.
            self.calls.set(self.calls.get() + 1000);
            panic!("seven is not allowed");
        }
        x + y
    }

    fn dpi_tick(&mut self) -> i32 {
        self.calls.set(self.calls.get() + 1);
        0
    }
}

fn dpi_panic(failure: &Failure) -> &str {
    match &failure.kind {
        FailureKind::DpiPanic { message } => message,
        other => panic!("expected DpiPanic, got {other:?}"),
    }
}

#[test]
fn a_panic_fails_the_run_and_poisons_the_model() {
    let (mut sim, dut) = sim::<dpi::Dut>();
    let calls = Rc::new(Cell::new(0));
    dut.set_dpi(Picky {
        calls: Rc::clone(&calls),
    });
    let failure = sim
        .run(async move {
            dut.a.set(1);
            timer(TICK).await;
            dut.a.set(7);
            timer(TICK).await;
            cycles(dut.clk, 3).await;
        })
        .unwrap_err();
    let message = dpi_panic(&failure);
    assert!(
        message.starts_with("DPI-C import `dpi_add` of DUT `dpi` panicked"),
        "{message}"
    );
    let location = failure.location.expect("the panicking line");
    assert!(
        location
            .to_string()
            .starts_with("crates/simjig/tests/dpi_tests/failures.rs:"),
        "{location}"
    );
    assert!(
        message.contains(&format!(
            "panicked at {location}: seven is not allowed. The design received zero"
        )),
        "{message}"
    );
    assert_eq!(failure.time.ticks(), 1, "the eval that applied a = 7");
    // The panicking call was the last one: the model was not evaluated again, and later
    // calls in the same eval (if any) did not reach the implementation.
    let calls = calls.get();
    assert!(calls > 1000 && calls < 2000, "calls = {calls}");
}

#[simjig::test]
#[should_panic(expected = "seven is not allowed")]
async fn a_panic_in_a_harness_test_is_the_failure_report(dut: dpi::Dut) {
    dut.set_dpi(Picky {
        calls: Rc::new(Cell::new(0)),
    });
    dut.a.set(7);
    timer(TICK).await;
    panic!("not reached: the run fails at the eval");
}

/// Runs `misuse` inside every `dpi_tick` call.
struct Misuse(Box<dyn FnMut()>);

impl dpi::Dpi for Misuse {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x + y
    }

    fn dpi_tick(&mut self) -> i32 {
        (self.0)();
        0
    }
}

/// Runs one clock cycle with `misuse` inside `dpi_tick`; returns the `DpiPanic` message.
fn misuse(misuse: impl FnOnce(dpi::Dut) -> Box<dyn FnMut()>) -> String {
    let (mut sim, dut) = sim::<dpi::Dut>();
    dut.set_dpi(Misuse(misuse(dut)));
    let failure = sim
        .run(async move { cycles(dut.clk, 1).await })
        .unwrap_err();
    dpi_panic(&failure).to_owned()
}

/// The rule's message for `what`, as the kernel words it.
fn rule(what: &str) -> String {
    format!(
        "DPI-C import `dpi_tick` used {what}: DPI functions run inside eval; they can't \
         access signals or await"
    )
}

#[test]
fn reading_a_signal_inside_a_dpi_call_fails_with_the_rule() {
    let message = misuse(|dut| {
        Box::new(move || {
            let _value = dut.sum.get();
        })
    });
    assert!(message.contains(&rule("a signal")), "{message}");
}

#[test]
fn writing_a_signal_inside_a_dpi_call_fails_with_the_rule() {
    let message = misuse(|dut| Box::new(move || dut.a.set(3)));
    assert!(message.contains(&rule("a signal")), "{message}");
}

#[test]
fn arming_a_trigger_inside_a_dpi_call_fails_with_the_rule() {
    let message = misuse(|dut| Box::new(move || drop(dut.clk.rising())));
    assert!(message.contains("`dpi_tick` used "), "{message}");
    assert!(
        message.contains("DPI functions run inside eval; they can't access signals or await"),
        "{message}"
    );
}

#[test]
fn spawning_inside_a_dpi_call_fails_with_the_rule() {
    let message = misuse(|_| Box::new(|| simjig::spawn("late", async {}).detach()));
    assert!(message.contains(&rule("spawn")), "{message}");
}

#[test]
fn reading_the_time_inside_a_dpi_call_fails_with_the_rule() {
    let message = misuse(|_| {
        Box::new(|| {
            let _now = simjig::now();
        })
    });
    assert!(message.contains(&rule("now()")), "{message}");
}

#[test]
fn awaiting_a_timer_inside_a_dpi_call_fails_with_the_rule() {
    let message = misuse(|_| {
        Box::new(|| {
            // What an `.await` does first: poll the timer once.
            let mut timer = std::pin::pin!(timer(TICK));
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let _poll = timer.as_mut().poll(&mut cx);
        })
    });
    assert!(message.contains("`dpi_tick` used "), "{message}");
    assert!(
        message.contains("DPI functions run inside eval; they can't access signals or await"),
        "{message}"
    );
}

/// `text` with every model prefix's hash (`Vdpi_05208168`) masked, so a snapshot does not
/// change with the prefix scheme.
pub(crate) fn mask_prefixes(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("`V") {
        let (head, tail) = rest.split_at(at + 2);
        out.push_str(head);
        let name_end = tail.find('`').unwrap_or(tail.len());
        let name = &tail[..name_end];
        match name.rsplit_once('_') {
            Some((stem, hash))
                if hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                out.push_str(stem);
                out.push_str("_<hash>");
            }
            _ => out.push_str(name),
        }
        rest = &tail[name_end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn a_missing_implementation_is_a_setup_failure_that_says_how_to_register_one() {
    let (mut sim, _dut) = sim::<dpi::Dut>();
    let failure = sim.run(async move { timer(TICK).await }).unwrap_err();
    assert_eq!(failure.kind.name(), "Setup");
    assert!(failure.message.contains("DUT `dpi`"), "{failure}");
    assert!(
        !failure.message.contains("set_dpi"),
        "the help alone says what to do"
    );
    insta::assert_snapshot!(mask_prefixes(&failure.to_string()));
}

#[test]
fn two_instances_of_one_dpi_build_fail_with_the_workaround() {
    let (mut sim, (a, _b)) = sim::<(dpi::Dut, dpi::Dut)>();
    a.set_dpi(Picky {
        calls: Rc::new(Cell::new(0)),
    });
    let failure = sim.run(async move { timer(TICK).await }).unwrap_err();
    assert_eq!(failure.kind.name(), "Setup");
    insta::assert_snapshot!(mask_prefixes(&failure.to_string()));
}

#[test]
fn forking_inside_a_dpi_call_fails_with_the_rule() {
    let message = misuse(|_| {
        Box::new(|| {
            let seed = simjig::Seed::new(1, simjig::SeedSource::Env);
            let checkpoint = simjig::fork::run_from_checkpoint(seed, 1, |_| async { Ok(()) });
            let mut checkpoint = std::pin::pin!(checkpoint);
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let _poll = checkpoint.as_mut().poll(&mut cx);
        })
    });
    assert!(
        message.contains(&rule("fork::run_from_checkpoint")),
        "{message}"
    );
}
