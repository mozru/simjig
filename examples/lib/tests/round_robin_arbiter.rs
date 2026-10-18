//! Testbench for `rtl/round_robin_arbiter.sv`, built with `N = 5`.
//!
//! The split is the usual one:
//! - a **driver** (the test body) sets `req` and `hold` once per cycle, at the falling
//!   edge, so the values are in place for the next rising edge;
//! - a **monitor** task samples the ports at every rising edge, checks the per-cycle
//!   properties and a reference model of the priority pointer, and logs each sample;
//! - each test then checks its own property (fairness, the starvation bound) over the
//!   log.
//!
//! `rising()` of a testbench clock sees the pre-edge values, so a sample holds the `req`
//! and `gnt` that the edge acts on; the driver's writes half a cycle later are what the
//! next edge sees.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;

use simjig::{Rng, TestCtx, clock, cycles, ns, spawn};

mod common;

simjig::include_dut!(round_robin_arbiter);

use round_robin_arbiter::Dut;

/// The number of requesters the build script chose (`.param("N", 5)`).
const N: usize = Dut::N as usize;

/// Every requester requesting; request and grant vectors are `u128`s, one bit per requester.
const ALL: u128 = (1 << N) - 1;

/// The ports at one rising edge, before the edge acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    req: u128,
    hold: bool,
    gnt: u128,
}

/// The arbiter's priority pointer, modelled in Rust.
#[derive(Debug, Default)]
struct PointerModel {
    /// The requester with the highest priority this cycle.
    ptr: usize,
}

impl PointerModel {
    /// The requester granted for `req`: the first one at or after the pointer.
    fn winner(&self, req: u128) -> Option<usize> {
        (0..N)
            .map(|i| (self.ptr + i) % N)
            .find(|&c| req >> c & 1 == 1)
    }

    /// The pointer after a rising edge: just past the winner, or at it under `hold`.
    fn step(&mut self, req: u128, hold: bool) {
        if let Some(w) = self.winner(req) {
            self.ptr = if hold { w } else { (w + 1) % N };
        }
    }
}

/// Every sample the monitor took, in order.
type Log = Rc<RefCell<Vec<Sample>>>;

/// Starts the clock and holds the arbiter in its synchronous reset for two cycles, with
/// no requests. Releases it at the falling edge after the second and returns there, so
/// the caller's first [`drive`] is seen by the next edge, the first out of reset.
async fn reset(dut: Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst_n.set(0);
    dut.req.set(0);
    dut.hold.set(0);
    cycles(dut.clk, 2).await;
    dut.clk.falling().await;
    dut.rst_n.set(1);
}

/// Drives one cycle, called at a falling edge: sets the inputs, waits for the rising edge
/// that acts on them, and returns the `gnt` that edge saw. Returns at the next falling
/// edge, where the next cycle's drive goes.
async fn drive(dut: Dut, req: u128, hold: bool) -> u128 {
    dut.req.set(req);
    dut.hold.set(hold);
    dut.clk.rising().await;
    let gnt = dut.gnt.get().to_u128();
    dut.clk.falling().await;
    gnt
}

/// Spawns the monitor: at every rising edge out of reset it checks that
/// - `gnt` is one-hot or zero,
/// - `gnt` goes only to a requester,
/// - something is granted whenever something requests,
/// - `gnt` is what the pointer model predicts,
///
/// then steps the model and logs the sample. A failed check panics in the task
/// `monitor`, which fails the test with the cycle's values.
fn spawn_monitor(dut: Dut) -> Log {
    let log = Log::default();
    let samples = Rc::clone(&log);
    spawn("monitor", async move {
        let mut model = PointerModel::default();
        loop {
            dut.clk.rising().await;
            if !dut.rst_n.get().to_bool() {
                model = PointerModel::default();
                continue;
            }
            let sample = Sample {
                req: dut.req.get().to_u128(),
                hold: dut.hold.get().to_bool(),
                gnt: dut.gnt.get().to_u128(),
            };
            check_cycle(&model, sample);
            model.step(sample.req, sample.hold);
            samples.borrow_mut().push(sample);
        }
    })
    .detach();
    log
}

/// The per-cycle properties and the reference model, for one sample.
#[track_caller]
fn check_cycle(model: &PointerModel, s: Sample) {
    assert!(s.gnt.count_ones() <= 1, "gnt is not one-hot or zero: {s:?}");
    assert_eq!(s.gnt & !s.req, 0, "a grant without a request: {s:?}");
    assert!(s.req == 0 || s.gnt != 0, "requests but no grant: {s:?}");
    let expected = model.winner(s.req).map_or(0, |w| 1 << w);
    assert_eq!(
        s.gnt, expected,
        "gnt differs from the model (pointer at {}): {s:?}",
        model.ptr
    );
}

/// A random request vector.
fn random_req(rng: &mut Rng) -> u128 {
    u128::from(rng.next_u64()) & ALL
}

/// Random requests and random `hold`, checked cycle by cycle against the pointer model.
#[simjig::test(timeout = "100us")]
async fn grants_match_the_pointer_model(ctx: &TestCtx, dut: Dut) {
    let mut rng = ctx.rng("stim");
    let log = spawn_monitor(dut);
    reset(dut).await;
    for _ in 0..2000 {
        // Hold one cycle in four, so locked grants and rotation both occur.
        let hold = rng.below(4) == 0;
        drive(dut, random_req(&mut rng), hold).await;
    }
    let log = log.borrow();
    assert_eq!(log.len(), 2000);
    assert!(
        log.iter().any(|s| s.hold && s.gnt != 0),
        "no locked grant ran"
    );
}

/// Fairness: with every requester requesting, each is granted exactly once in every
/// window of `N` consecutive cycles, in rotating order from requester 0.
#[simjig::test(timeout = "10us")]
async fn all_requesting_grants_each_once_per_n_cycles(dut: Dut) {
    let log = spawn_monitor(dut);
    reset(dut).await;
    for _ in 0..4 * N {
        drive(dut, ALL, false).await;
    }
    let grants: Vec<u128> = log.borrow().iter().map(|s| s.gnt).collect();
    for window in grants.windows(N) {
        let union = window.iter().fold(0, |acc, g| acc | g);
        assert_eq!(
            union, ALL,
            "a window of {N} cycles missed a requester: {window:?}"
        );
    }
    let order: Vec<u32> = grants.iter().map(|g| g.trailing_zeros()).collect();
    let expected: Vec<u32> = (0..4 * Dut::N).map(|i| i % Dut::N).collect();
    assert_eq!(order, expected, "grants rotate 0, 1, ..., N-1, 0, ...");
}

/// The starvation bound: a requester that holds its request until granted is granted
/// within `N` cycles, under random arrivals and without `hold`.
#[simjig::test(timeout = "100us")]
async fn a_held_request_is_granted_within_n_cycles(ctx: &TestCtx, dut: Dut) {
    let mut rng = ctx.rng("arrivals");
    let log = spawn_monitor(dut);
    reset(dut).await;
    let mut req = 0;
    for _ in 0..2000 {
        // A pending request stays; a new one arrives with probability 1/2 per requester.
        req |= random_req(&mut rng);
        // The granted requester drops its request.
        req &= !drive(dut, req, false).await;
    }
    let max_wait = longest_wait(&log.borrow());
    assert!(
        max_wait <= N,
        "a request waited {max_wait} cycles; the bound is {N}"
    );
    assert_eq!(max_wait, N, "the stimulus never reached the worst case");
}

/// The longest run of cycles any requester requested, up to and including its grant.
fn longest_wait(samples: &[Sample]) -> usize {
    let mut waiting = [0usize; N];
    let mut longest = 0;
    for s in samples {
        for (r, wait) in waiting.iter_mut().enumerate() {
            if s.req >> r & 1 == 1 {
                *wait += 1;
                longest = longest.max(*wait);
            }
            if s.gnt >> r & 1 == 1 || s.req >> r & 1 == 0 {
                *wait = 0;
            }
        }
    }
    longest
}

/// `hold` keeps the grant with its owner for as long as the owner requests; releasing it
/// moves the grant on.
#[simjig::test(timeout = "10us")]
async fn hold_keeps_the_grant_with_its_owner(dut: Dut) {
    let log = spawn_monitor(dut);
    reset(dut).await;
    drive(dut, ALL, false).await; // 0 wins; the pointer moves past it, to 1
    for _ in 0..5 {
        drive(dut, ALL, true).await; // 1 wins and keeps the pointer
    }
    drive(dut, ALL, false).await; // 1 again, then released: the pointer moves to 2
    drive(dut, ALL & !0b100, true).await; // 2 does not request: 3 wins and locks
    drive(dut, ALL, true).await; // 3 keeps the grant over 2
    let grants: Vec<u128> = log.borrow().iter().map(|s| s.gnt).collect();
    assert_eq!(grants, [1, 2, 2, 2, 2, 2, 2, 8, 8]);
}

/// A reset in the middle of random traffic, with the pointer away from 0 (Verilator
/// starts it at 0, so only a reset from elsewhere shows the reset clears it): with every
/// requester requesting through the reset edge, the first grant after it goes to
/// requester 0, and random traffic then matches the model, which the monitor restarts at
/// the reset edge.
#[simjig::test(timeout = "100us")]
async fn a_reset_during_traffic_points_the_priority_at_0(ctx: &TestCtx, dut: Dut) {
    let mut rng = ctx.rng("stim");
    let log = spawn_monitor(dut);
    reset(dut).await;
    for _ in 0..200 {
        drive(dut, random_req(&mut rng), rng.below(4) == 0).await;
    }
    drive(dut, 0b00100, false).await; // 2 wins: the pointer moves to 3
    dut.rst_n.set(0);
    let before = drive(dut, ALL, false).await; // the reset edge
    assert_eq!(before, 1 << 3, "the pointer was at 3 when reset came");
    dut.rst_n.set(1);
    let after = drive(dut, ALL, false).await;
    assert_eq!(
        after, 1,
        "the first grant after the reset goes to requester 0"
    );
    for _ in 0..200 {
        drive(dut, random_req(&mut rng), rng.below(4) == 0).await;
    }
    // Every edge but the reset edge was checked and logged.
    assert_eq!(log.borrow().len(), 200 + 1 + 1 + 200);
}

/// The elaboration check: any N from 2 up elaborates (a power of 2 or not), and N below 2
/// stops elaboration.
#[test]
fn parameters_are_checked_at_elaboration() {
    let src = &["rtl/round_robin_arbiter.sv"];
    let good: &[&[&str]] = &[&["N=2"], &["N=7"], &["N=32"]];
    let bad: &[&[&str]] = &[&["N=0"], &["N=1"]];
    common::assert_param_checks(
        "round_robin_arbiter",
        src,
        good,
        bad,
        "N must be at least 2",
    );
}
