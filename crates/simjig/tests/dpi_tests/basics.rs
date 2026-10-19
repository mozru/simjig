//! Results flow back into the RTL, through `Sim` and through `#[simjig::test]`; two model
//! instances with different implementations never see each other's.

use std::cell::Cell;
use std::rc::Rc;

use simjig::{read_only, timer};
use simjig_bits::Bits;

use super::{TICK, cycles, dpi, sim};

/// `dpi_add` plus an offset; counts `dpi_tick` calls in a cell the test keeps.
struct Adder {
    offset: u32,
    ticks: Rc<Cell<i32>>,
}

impl Adder {
    fn new(offset: u32) -> (Self, Rc<Cell<i32>>) {
        let ticks = Rc::new(Cell::new(0));
        let adder = Self {
            offset,
            ticks: Rc::clone(&ticks),
        };
        (adder, ticks)
    }
}

impl dpi::Dpi for Adder {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x.wrapping_add(y).wrapping_add_unsigned(self.offset)
    }

    fn dpi_tick(&mut self) -> i32 {
        self.ticks.set(self.ticks.get() + 1);
        self.ticks.get() * 10
    }
}

/// Sets `a` and `b`, settles, cycles the clock `n` times; returns `(sum, calls)` as of
/// the end of that time step, and leaves it.
async fn drive(dut: &dpi::Dut, a: u32, b: u32, n: u32) -> (Bits<32>, Bits<32>) {
    dut.a.set(u128::from(a));
    dut.b.set(u128::from(b));
    timer(TICK).await;
    cycles(dut.clk, n).await;
    read_only().await;
    let seen = (dut.sum.get(), dut.calls.get());
    timer(TICK).await;
    seen
}

#[test]
fn results_flow_back_into_the_rtl() {
    let (mut sim, dut) = sim::<dpi::Dut>();
    let (adder, ticks) = Adder::new(0);
    // Registered before the run: the first eval already has it.
    dut.set_dpi(adder);
    let (sum, calls) = sim.run(async move { drive(&dut, 2, 3, 5).await }).unwrap();
    assert_eq!(sum, 5, "sum = dpi_add(a, b), combinationally");
    assert_eq!(calls, 50, "calls <= dpi_tick() on each posedge");
    assert_eq!(ticks.get(), 5);
}

#[test]
fn signed_results_wrap_like_the_c_reference() {
    let (mut sim, dut) = sim::<dpi::Dut>();
    dut.set_dpi(Adder::new(0).0);
    let (sum, _) = sim
        .run(async move { drive(&dut, 0xffff_fff0, 0x20, 0).await })
        .unwrap();
    assert_eq!(
        sum, 0x10,
        "int wraps at 32 bits (dpi_impl.c adds as uint32_t)"
    );
}

#[simjig::test]
async fn an_implementation_registered_at_the_start_of_a_test_serves_time_zero(dut: dpi::Dut) {
    let (adder, ticks) = Adder::new(1000);
    dut.set_dpi(adder);
    let (sum, calls) = drive(&dut, 1, 2, 3).await;
    assert_eq!((sum, calls), (Bits::new(1003), Bits::new(30)));
    assert_eq!(ticks.get(), 3);
}

#[test]
fn replacing_the_implementation_takes_effect_on_the_next_call() {
    let (mut sim, dut) = sim::<dpi::Dut>();
    dut.set_dpi(Adder::new(0).0);
    let sums = sim
        .run(async move {
            let (first, _) = drive(&dut, 1, 1, 0).await;
            dut.set_dpi(Adder::new(100).0);
            // Same inputs: the comb block does not re-run until an input changes.
            let (second, _) = drive(&dut, 2, 1, 0).await;
            (first, second)
        })
        .unwrap();
    assert_eq!(sums, (Bits::new(2), Bits::new(103)));
}

#[test]
fn two_instances_with_different_implementations_stay_isolated() {
    let (mut sim_a, dut_a) = sim::<dpi::Dut>();
    let (mut sim_b, dut_b) = sim::<dpi::Dut>();
    let (adder_a, ticks_a) = Adder::new(100);
    let (adder_b, ticks_b) = Adder::new(200);
    dut_a.set_dpi(adder_a);
    dut_b.set_dpi(adder_b);
    let a = sim_a
        .run(async move { drive(&dut_a, 1, 2, 3).await })
        .unwrap();
    let b = sim_b
        .run(async move { drive(&dut_b, 1, 2, 7).await })
        .unwrap();
    assert_eq!(a, (Bits::new(103), Bits::new(30)));
    assert_eq!(b, (Bits::new(203), Bits::new(70)));
    assert_eq!((ticks_a.get(), ticks_b.get()), (3, 7));
}

/// Two instances evaluated at the same time on two threads: each thread's evals dispatch
/// to its own instance's implementation, call after call.
#[test]
fn instances_on_two_threads_interleave_without_crosstalk() {
    let run = |offset: u32| {
        std::thread::spawn(move || {
            let (mut sim, dut) = sim::<dpi::Dut>();
            let (adder, ticks) = Adder::new(offset);
            dut.set_dpi(adder);
            let sums = sim
                .run(async move {
                    let mut sums = Vec::new();
                    for i in 0..2000 {
                        sums.push(drive(&dut, i, 1, 1).await.0.to_u128());
                    }
                    sums
                })
                .unwrap();
            (sums, ticks.get())
        })
    };
    let (a, b) = (run(1_000_000), run(2_000_000));
    let (sums_a, ticks_a) = a.join().unwrap();
    let (sums_b, ticks_b) = b.join().unwrap();
    for i in 0..2000_u32 {
        assert_eq!(sums_a[i as usize], u128::from(i + 1 + 1_000_000));
        assert_eq!(sums_b[i as usize], u128::from(i + 1 + 2_000_000));
    }
    assert_eq!((ticks_a, ticks_b), (2000, 2000));
}
