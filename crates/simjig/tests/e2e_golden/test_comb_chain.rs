//! `tests/conformance/tb/test_comb_chain.py`. `s1`..`s3` are internal nets, read through
//! the read-only probes `build.rs` declares.

use simjig::{ns, read_only, read_write, spawn, timer};

use crate::comb_chain::Dut;
use crate::conflog::Log;

#[simjig::test]
async fn comb_settle(dut: Dut) {
    let Dut {
        a,
        b,
        y,
        y_eq,
        s1,
        s2,
        s3,
    } = dut;
    let log = Log::default();
    let mut p = log.probe("main");
    let mut w = log.probe("y_watch");
    p.write(&a, 0);
    p.write(&b, 0);
    p.wait(timer(ns(1)), "Timer(1ns)").await;
    p.read(&[&a, &b, &y, &y_eq]);
    let watcher = spawn("y_watch", async move {
        for _ in 0..3 {
            w.wait(y.changed(), "ValueChange(y)").await;
            w.read(&[&a, &y]);
        }
    });
    for (va, vb) in [(0x10, 0), (0x20, 3), (0xff, 0x40)] {
        p.write(&a, va);
        p.write(&b, vb);
        p.read(&[&a, &y]);
        p.wait(read_write(), "ReadWrite").await;
        p.read(&[&a, &y]);
        p.wait(read_only(), "ReadOnly").await;
        p.read(&[&a, &b, &y, &y_eq, &s1, &s2, &s3]);
        p.wait(timer(ns(1)), "Timer(1ns)").await;
        p.read(&[&a, &y]);
    }
    p.wait(read_write(), "ReadWrite").await;
    p.write(&a, 0x01);
    p.read(&[&a, &y]);
    p.wait(read_only(), "ReadOnly").await;
    p.read(&[&a, &y]);
    watcher.await;
    log.check("comb_settle");
}
