//! A synchronous test: a plain `fn` that steps one clock, like a hand-written Verilator
//! loop, with no scheduler in between.

use quickstart::*;

#[simjig::test(sync)]
fn q_follows_d_sync(dut: flop::Dut) {
    let mut cyc = SyncClock::new(dut.clk, ns(10));
    let q = cyc.bind(dut.q); // a bound handle: a direct read, no lookup
    for d in [true, false, true] {
        dut.d.set(d);
        cyc.tick(); // rise, eval, fall, eval
        assert_eq!(q.get(), d);
    }
}
