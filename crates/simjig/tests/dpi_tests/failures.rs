//! A DPI implementation that panics, is missing, or touches a signal fails the run with a
//! report; the process does not abort, and the model is not evaluated again.

use std::cell::Cell;
use std::rc::Rc;

use simjig::__private::Out;
use simjig::{Failure, FailureKind, Timer};
use simjig_bits::Bits;

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

fn model_fatal(failure: &Failure) -> &str {
    match &failure.kind {
        FailureKind::ModelFatal { message } => message,
        other => panic!("expected ModelFatal, got {other:?}"),
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
            Timer::new(TICK).await;
            dut.a.set(7);
            Timer::new(TICK).await;
            cycles(dut.clk, 3).await;
        })
        .unwrap_err();
    let message = model_fatal(&failure);
    assert!(
        message.starts_with("DPI-C import `dpi_add` of model `Vdpi_"),
        "{message}"
    );
    assert!(
        message.contains("panicked: seven is not allowed. The design received zero"),
        "{message}"
    );
    assert_eq!(failure.time.ticks(), 1, "the eval that applied a = 7");
    // The panicking call was the last one: the model was not evaluated again, and later
    // calls in the same eval (if any) did not reach the implementation.
    let calls = calls.get();
    assert!(calls > 1000 && calls < 2000, "calls = {calls}");
}

#[simjig::test]
#[should_panic(expected = "panicked: seven is not allowed")]
async fn a_panic_in_a_harness_test_is_the_failure_report(dut: dpi::Dut) {
    dut.set_dpi(Picky {
        calls: Rc::new(Cell::new(0)),
    });
    dut.a.set(7);
    Timer::new(TICK).await;
    panic!("not reached: the run fails at the eval");
}

#[test]
fn a_call_without_an_implementation_says_how_to_register_one() {
    let (mut sim, _dut) = sim::<dpi::Dut>();
    let failure = sim.run(async move { Timer::new(TICK).await }).unwrap_err();
    let message = model_fatal(&failure);
    assert!(
        message.contains("has no DPI implementation; call `dut.set_dpi(..)` before the first eval"),
        "{message}"
    );
    assert_eq!(failure.time.ticks(), 0, "the initial eval computes sum");
}

/// Reads a signal from inside `dpi_tick`.
struct Peek {
    sum: Out<Bits<32>>,
}

impl dpi::Dpi for Peek {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x + y
    }

    fn dpi_tick(&mut self) -> i32 {
        i32::try_from(self.sum.get().to_u128()).unwrap()
    }
}

#[test]
fn reading_a_signal_inside_a_dpi_call_fails_with_the_rule() {
    let (mut sim, dut) = sim::<dpi::Dut>();
    dut.set_dpi(Peek { sum: dut.sum });
    let failure = sim
        .run(async move { cycles(dut.clk, 1).await })
        .unwrap_err();
    let message = model_fatal(&failure);
    assert!(
        message.contains(
            "panicked: DPI-C import `dpi_tick` read a signal: DPI functions run inside eval; \
             they can't access signals or await"
        ),
        "{message}"
    );
}

/// Arms a trigger (the first step of an await) from inside `dpi_tick`.
struct Wait {
    clk: simjig::__private::In<simjig_bits::Bit>,
}

impl dpi::Dpi for Wait {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x + y
    }

    fn dpi_tick(&mut self) -> i32 {
        drop(self.clk.rising());
        0
    }
}

#[test]
fn awaiting_inside_a_dpi_call_fails_with_the_rule() {
    let (mut sim, dut) = sim::<dpi::Dut>();
    dut.set_dpi(Wait { clk: dut.clk });
    let failure = sim
        .run(async move { cycles(dut.clk, 1).await })
        .unwrap_err();
    assert!(
        model_fatal(&failure)
            .contains("`dpi_tick` awaited a signal: DPI functions run inside eval"),
        "{failure}"
    );
}
