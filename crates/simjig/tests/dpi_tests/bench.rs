//! Cost of a DPI call into Rust, end to end through a real model and the kernel: cycles of
//! `tests/duts/dpi` whose posedge eval calls `dpi_tick` once, against the same cycles with
//! the negedge-only eval (no call). Not a correctness test: run with
//! `cargo test --release -p simjig --test dpi_tests -- --ignored --nocapture measure`.
//! The trampoline alone (no C++ caller, no kernel) is measured by the `dpi::abi` unit
//! benchmark.

use std::time::Instant;

use simjig::Timer;

use super::{TICK, dpi, sim};

struct Count(i32);

impl dpi::Dpi for Count {
    fn dpi_add(&mut self, x: i32, y: i32) -> i32 {
        x.wrapping_add(y)
    }

    fn dpi_tick(&mut self) -> i32 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
}

const N: u32 = 200_000;

#[expect(
    clippy::disallowed_methods,
    reason = "wall-clock measurement, not simulation"
)]
#[test]
#[ignore = "measurement; run explicitly in release mode"]
fn measure_dpi_call_cost() {
    let (mut sim, dut) = sim::<dpi::Dut>();
    dut.set_dpi(Count(0));
    let (rise, fall) = sim
        .run(async move {
            let (mut rise, mut fall) = (0.0, 0.0);
            for _ in 0..N {
                let t = Instant::now();
                dut.clk.set(true);
                Timer::new(TICK).await;
                rise += t.elapsed().as_secs_f64();
                let t = Instant::now();
                dut.clk.set(false);
                Timer::new(TICK).await;
                fall += t.elapsed().as_secs_f64();
            }
            (rise, fall)
        })
        .unwrap();
    let per = |s: f64| s * 1e9 / f64::from(N);
    println!(
        "DPI: timestep with a posedge eval calling dpi_tick {:.1} ns, negedge timestep (no \
         call) {:.1} ns, difference {:.1} ns (one call plus the flop update)",
        per(rise),
        per(fall),
        per(rise) - per(fall)
    );
}
