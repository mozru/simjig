//! Uses `opt` behind its gate: builds whether or not it built.
#![deny(unexpected_cfgs)]

#[cfg(simjig_dut_opt)]
simjig::include_dut!(opt);

#[cfg(simjig_dut_opt)]
#[simjig::test]
async fn opt_built(dut: opt::Dut) {
    dut.a.set(0);
    simjig::Timer::new(simjig::ns(1)).await;
    assert_eq!(dut.y.get(), 1);
}

#[cfg(not(simjig_dut_opt))]
#[test]
fn opt_skipped() {}
