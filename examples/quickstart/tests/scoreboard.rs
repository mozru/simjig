//! Concurrency: a driver and a monitor as tasks, a seeded random stream, and a queue as
//! the scoreboard between them.

use quickstart::*;

#[simjig::test(timeout = "10us")]
async fn q_follows_random_d(ctx: &TestCtx, dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    reset(&dut).await;
    let sb = Queue::new("sb", 0); // 0: unbounded
    let expected = sb.clone();
    let mut rng = ctx.rng("stim"); // seeded: the report's seed replays it
    let driver = spawn("driver", async move {
        for _ in 0..100 {
            dut.d.set(rng.bool());
            dut.clk.rising().await; // the edge captures `d`
            expected.put(dut.d.get()).await;
        }
    });
    spawn("monitor", async move {
        loop {
            dut.clk.rising().read_only().await; // final values of the edge's timestep
            assert_eq!(dut.q.get(), sb.get().await);
        }
    })
    .detach(); // runs until the test ends
    with_timeout(us(2), driver).await.unwrap();
}
