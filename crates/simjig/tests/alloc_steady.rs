//! The scheduler's steady state allocates nothing: once a testbench has run a few cycles,
//! a timestep of clock edges, evals, value-change wakes, task polls and writes reuses the
//! buffers of the timesteps before it (docs/notes/perf.md, "Heap churn").
//!
//! A dedicated test binary because it installs a counting `#[global_allocator]`. The count
//! is per thread, so tests running in parallel in this binary do not see each other's
//! allocations; a simulation runs on one thread. Only Rust allocations are counted: the
//! C++ side of the model allocates through its own `operator new`.
//!
//! Each case warms up for [`WARMUP`] rising edges (growing every buffer to its working
//! size), then asserts that the next [`MEASURED`] edges allocate nothing, against the
//! Verilator `counter` model (`tests/duts/counter`).
#![expect(clippy::unwrap_used, reason = "test helpers outside #[test] functions")]
#![expect(unsafe_code, reason = "a GlobalAlloc impl is unsafe by definition")]

#[cfg(not(simjig_test_models))]
#[test]
fn models_were_built() {
    panic!(
        "build.rs could not build the test models: {}",
        env!("SIMJIG_TEST_MODELS_ERROR")
    );
}

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The system allocator, counting this thread's allocations and reallocations.
struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    // `try_with`: allocations during thread teardown, after the slot is gone, are not ours.
    ALLOCS.try_with(|n| n.set(n.get() + 1)).unwrap_or(());
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged, so the
// `GlobalAlloc` contract holds exactly as it does for `System`.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged (see the impl).
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged (see the impl).
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged (see the impl).
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: forwarded unchanged (see the impl).
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// This thread's allocations so far.
fn allocs() -> u64 {
    ALLOCS.with(Cell::get)
}

/// Rising edges before measuring: enough for every buffer to reach its working size.
const WARMUP: u32 = 64;
/// Rising edges measured.
const MEASURED: u32 = 1000;

#[cfg(simjig_test_models)]
simjig::include_dut!(counter);

#[cfg(simjig_test_models)]
mod cases {
    use simjig::{Sim, clock, spawn};

    use super::{MEASURED, WARMUP, allocs, counter};

    fn sim() -> (Sim, counter::Dut) {
        use simjig::DutModel as _;
        let options = simjig::ModelOptions::new(simjig::Seed::new(1, simjig::SeedSource::Env));
        let mut sim = counter::Dut::instantiate(&options).unwrap();
        let dut = counter::Dut::create(&mut sim).unwrap();
        (sim, dut)
    }

    /// Runs `body` (which returns the allocations it saw while measuring) and asserts
    /// there were none.
    fn assert_steady<F: Future<Output = u64> + 'static>(
        case: &str,
        body: impl FnOnce(counter::Dut) -> F,
    ) {
        let (mut sim, dut) = sim();
        let seen = sim.run(body(dut)).unwrap();
        assert_eq!(
            seen, 0,
            "{case}: {seen} allocations in {MEASURED} steady-state cycles"
        );
    }

    /// The kernel clock alone: the clock-only benchmark (bench/README.md, variant A).
    #[test]
    fn the_clock_alone_allocates_nothing_per_timestep() {
        assert_steady("clock only", |dut| async move {
            let clk = dut.clk;
            clock(clk, simjig::ns(10)).start();
            for _ in 0..WARMUP {
                clk.rising().await;
            }
            let before = allocs();
            for _ in 0..MEASURED {
                clk.rising().await;
            }
            allocs() - before
        });
    }

    /// A monitor task sampling at every rising edge while the main task waits on the
    /// same edge (two waiters, one group), and a driver writing at the falling edge: the
    /// shape of the full benchmark (variant C).
    #[test]
    fn a_monitor_and_a_driver_allocate_nothing_per_timestep() {
        assert_steady("monitor and driver", |dut| async move {
            let (clk, en, count) = (dut.clk, dut.en, dut.count);
            clock(clk, simjig::ns(10)).start();
            let monitor = spawn("monitor", async move {
                let mut sum = 0u128;
                loop {
                    clk.rising().await;
                    sum = sum.wrapping_add(count.get().to_u128());
                }
            });
            let driver = spawn("driver", async move {
                let mut on = false;
                loop {
                    clk.falling().await;
                    on = !on;
                    en.set(on);
                }
            });
            for _ in 0..WARMUP {
                clk.rising().await;
            }
            let before = allocs();
            for _ in 0..MEASURED {
                clk.rising().await;
            }
            let seen = allocs() - before;
            monitor.cancel();
            driver.cancel();
            seen
        });
    }
}
