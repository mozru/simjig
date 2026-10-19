//! DPI calls inside fork branches and inside combinator operands: each branch has its own
//! implementation, and the parent's is untouched.

use std::cell::Cell;
use std::rc::Rc;

use simjig::{TestCtx, ns, read_only, timer, with_timeout};

use super::{TICK, cycles, dpi};

struct Adder {
    offset: u32,
    ticks: Rc<Cell<i32>>,
}

impl dpi::Dpi for Adder {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x.wrapping_add(y).wrapping_add_unsigned(self.offset)
    }
    fn dpi_tick(&mut self) -> i32 {
        self.ticks.set(self.ticks.get() + 1);
        self.ticks.get()
    }
}

async fn sum_and_calls(dut: &dpi::Dut, n: u32) -> (u128, u128) {
    dut.a.set(2u128);
    dut.b.set(3u128);
    timer(TICK).await;
    // DPI calls (dpi_tick on each posedge) while the cycles run inside a combinator helper.
    with_timeout(ns(1000), cycles(dut.clk, n)).await.unwrap();
    read_only().await;
    let r = (dut.sum.get().to_u128(), dut.calls.get().to_u128());
    timer(TICK).await;
    r
}

#[simjig::test(timeout = "100us")]
async fn dpi_in_fork_branches_and_combinators(ctx: &TestCtx, dut: dpi::Dut) {
    let ticks = Rc::new(Cell::new(0));
    dut.set_dpi(Adder {
        offset: 0,
        ticks: Rc::clone(&ticks),
    });
    let before = sum_and_calls(&dut, 2).await;
    assert_eq!(before, (5, 2));
    let t2 = Rc::clone(&ticks);
    let branches = simjig::fork::run_from_checkpoint(ctx.seed(), 2, move |b| {
        let t2 = Rc::clone(&t2);
        async move {
            if b.index() == 1 {
                dut.set_dpi(Adder {
                    offset: 100,
                    ticks: Rc::new(Cell::new(50)),
                });
            }
            let r = sum_and_calls(&dut, 3).await;
            Ok((r.0, r.1, u128::try_from(t2.get()).unwrap()))
        }
    })
    .await
    .unwrap();
    let values: Vec<(u128, u128, u128)> = branches.into_values();
    eprintln!("{values:?}");
    assert_eq!(
        values[0],
        (5, 5, 5),
        "branch 0 keeps the inherited implementation"
    );
    // Branch 1's dpi_tick is its own (53); `sum` is not re-evaluated (pure import, inputs
    // unchanged), so it still shows the inherited result.
    assert_eq!(
        values[1],
        (5, 53, 2),
        "branch 1 its own; the inherited cell untouched"
    );
    // The parent saw none of the branches' calls, and its implementation is still the first.
    assert_eq!(ticks.get(), 2);
    assert_eq!(sum_and_calls(&dut, 1).await, (5, 3));
}
