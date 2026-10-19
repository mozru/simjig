//! `tests/conformance/tb/test_fifo.py` (`DEPTH=4`): a valid/ready stream with back-pressure.

use simjig::{read_only, spawn};
use simjig_bits::Bit;

use crate::conflog::Log;
use crate::fifo::Dut;
use crate::start_clock;

const DATA: [u128; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
const READY: [u128; 16] = [0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1];

#[simjig::test]
async fn fifo_stream(dut: Dut) {
    let log = Log::default();
    let mut drv = log.probe("driver");
    let mut snk = log.probe("sink");
    let mut mon = log.probe("monitor");
    dut.clk.set(0);
    start_clock(dut.clk, 10);
    drv.write(&dut.rst, 1);
    drv.write(&dut.in_valid, 0);
    drv.write(&dut.in_data, 0);
    drv.write(&dut.out_ready, 0);
    drv.wait(dut.clk.rising(), "RisingEdge(clk)").await;
    drv.wait(dut.clk.falling(), "FallingEdge(clk)").await;
    drv.write(&dut.rst, 0);

    let drive = async move {
        let mut i = 0;
        while i < DATA.len() {
            drv.write(&dut.in_valid, 1);
            drv.write(&dut.in_data, DATA[i]);
            drv.wait(read_only(), "ReadOnly").await;
            drv.read(&[&dut.in_ready]);
            let moved = dut.in_ready.get() == Bit::ONE;
            drv.wait(dut.clk.falling(), "FallingEdge(clk)").await;
            i += usize::from(moved);
        }
        drv.write(&dut.in_valid, 0);
    };
    let consume = async move {
        for ready in READY {
            snk.wait(dut.clk.falling(), "FallingEdge(clk)").await;
            snk.write(&dut.out_ready, ready);
            snk.wait(read_only(), "ReadOnly").await;
            if ready == 1 && dut.out_valid.get() == Bit::ONE {
                snk.read(&[&dut.out_data]);
            }
        }
    };
    let observe = async move {
        for _ in 0..READY.len() {
            mon.wait(dut.clk.rising(), "RisingEdge(clk)").await;
            mon.read(&[
                &dut.in_valid,
                &dut.in_ready,
                &dut.out_valid,
                &dut.out_ready,
                &dut.level,
            ]);
        }
    };
    let m = spawn("monitor", observe);
    let s = spawn("sink", consume);
    let d = spawn("driver", drive);
    m.await;
    s.await;
    d.await;
    log.check("fifo_stream");
}
