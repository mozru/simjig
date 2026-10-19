//! A clocked design: a kernel-driven clock, and the value a clock edge produced, read in
//! the same timestep with `.read_write()`.

use quickstart::*;

#[simjig::test(timeout = "1us")]
async fn q_follows_d(dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    reset(&dut).await;
    for d in [true, false, true] {
        dut.d.set(d);
        // After `.read_write()` the flop shows what the edge captured.
        dut.clk.rising().read_write().await;
        assert_eq!(dut.q.get(), d);
    }
}
