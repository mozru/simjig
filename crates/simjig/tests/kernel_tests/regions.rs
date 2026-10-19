//! `.read_write()` and `.read_only()` on triggers (`TriggerExt`): the trigger, then that
//! region of the same timestep.

use simjig::{
    FailureKind, TriggerExt, cycles, falling, first, now, read_only, rising, timer, with_timeout,
};

use super::{assert_clean, clocked, ticks};

#[test]
fn after_rising_read_write_a_flop_shows_its_new_value_and_writes_are_allowed() {
    let mut d = clocked();
    let (clk, a, b, q, sum) = (d.clk, d.a, d.b, d.q, d.sum);
    let seen = d.sim.run(async move {
        timer(ticks(5)).await;
        a.write_u64(7);
        rising(clk).await;
        let before = (now().ticks(), q.read_u64()); // the edge at 10 has not acted yet
        a.write_u64(9);
        rising(clk).read_write().await;
        let after = (now().ticks(), q.read_u64()); // the edge at 20 sampled a = 9
        b.write_u64(1); // allowed: this is the ReadWrite region
        read_only().await;
        (before, after, now().ticks(), sum.read_u64())
    });
    assert_eq!(seen.unwrap(), ((10, 0), (20, 9), 20, 10));
    assert_clean(&d.sim);
}

#[test]
fn after_read_only_a_write_fails_and_the_help_names_read_only() {
    let mut d = clocked();
    let (clk, a) = (d.clk, d.a);
    let failure = d
        .sim
        .run(async move {
            timer(ticks(1)).await;
            falling(clk).read_only().await;
            a.write_u64(1);
        })
        .unwrap_err();
    assert!(
        matches!(&failure.kind, FailureKind::WriteInReadOnly { .. }),
        "{failure}"
    );
    assert_eq!(failure.time.ticks(), 5, "{failure}");
    assert!(failure.to_string().contains("`.read_only()`"), "{failure}");
}

#[test]
fn cycles_read_write_lands_in_the_timestep_of_the_nth_edge() {
    let mut d = clocked();
    let (clk, a, q) = (d.clk, d.a, d.q);
    let seen = d.sim.run(async move {
        timer(ticks(1)).await;
        a.write_u64(4);
        cycles(clk, 3).read_write().await; // edges at 10, 20, 30
        (now().ticks(), q.read_u64())
    });
    assert_eq!(seen.unwrap(), (30, 4));
    assert_clean(&d.sim);
}

#[test]
fn timer_read_write_and_read_only_stay_in_timestep_now_plus_t() {
    let mut d = clocked();
    let (a, b, sum) = (d.a, d.b, d.sum);
    let seen = d.sim.run(async move {
        timer(ticks(2)).await;
        a.write_u64(2);
        b.write_u64(3);
        timer(ticks(7)).read_write().await;
        let rw = (now().ticks(), sum.read_u64());
        b.write_u64(4);
        timer(ticks(3)).read_only().await;
        (rw, now().ticks(), sum.read_u64())
    });
    assert_eq!(seen.unwrap(), ((9, 5), 12, 6));
    assert_clean(&d.sim);
}

#[test]
fn a_dropped_region_suffix_leaves_nothing_registered() {
    // Loser while still on its trigger (the edge), and loser while on its region: both
    // Timers fire at 25, the ReadWrite region comes before the ReadOnly one.
    let mut d = clocked();
    let clk = d.clk;
    let seen = d.sim.run(async move {
        timer(ticks(1)).await;
        let on_edge = first!(
            async {
                rising(clk).read_write().await;
                "edge"
            },
            async {
                timer(ticks(3)).await;
                "timer"
            }
        );
        let on_region = first!(
            async {
                timer(ticks(21)).read_only().await;
                "read_only"
            },
            async {
                timer(ticks(21)).read_write().await;
                "read_write"
            }
        );
        let timed_out = with_timeout(ticks(2), cycles(clk, 1).read_only()).await;
        (on_edge, on_region, timed_out.is_err(), now().ticks())
    });
    assert_eq!(seen.unwrap(), ("timer", "read_write", true, 27));
    assert_clean(&d.sim);
}
