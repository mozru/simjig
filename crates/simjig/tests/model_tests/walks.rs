//! Value-change walks against what an eval can change: an input too, in a design that
//! writes its own input (its `ASSIGNIN` lint waived).

use simjig::{SimTime, clock, now, ns};

use super::duts::sim;
use crate::writes_input;

/// A waiter on an input that the design changes in an eval wakes in the timestep of that
/// eval, at each rising clock edge, not at the next write half a period later.
#[test]
fn a_change_the_design_makes_to_its_input_wakes_the_waiter_at_once() {
    let (mut sim, dut) = sim::<writes_input::Dut>();
    let times = sim
        .run(async move {
            clock(dut.clk, ns(10)).start();
            let mut times = Vec::new();
            for _ in 0..3 {
                dut.a.changed().await;
                times.push(now());
            }
            times
        })
        .unwrap();
    // The clock rises at 0, 10 and 20 ns (ticks of 1 ps).
    assert_eq!(times, [SimTime(0), SimTime(10_000), SimTime(20_000)]);
}
