//! A clocked design: a kernel-driven clock, and the value a clock edge produced, read in
//! the same timestep with `.read_write()`.

use quickstart::*;

#[simjig::test(timeout = "1us")]
async fn q_follows_d(dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    reset(&dut).await;
    for d in [true, false, true] {
        dut.d.set(d);
        // At the edge itself the flop has not updated yet; in the ReadWrite region of that
        // timestep it has, and writes are still allowed (the next `d`).
        dut.clk.rising().read_write().await;
        assert_eq!(dut.q.get(), Bit::from(d));
    }
    // The falling-edge convention instead: `dut.clk.falling().await` before each `set`,
    // and plain `dut.clk.rising().await` to sample what the edge acts on.
}
