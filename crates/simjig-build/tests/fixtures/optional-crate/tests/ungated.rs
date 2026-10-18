//! Uses `opt` without a gate: when it failed to build, this must not compile.

simjig::include_dut!(opt);

#[simjig::test]
async fn opt_inverts(dut: opt::Dut) {
    dut.a.set(1);
    simjig::Timer::new(simjig::ns(1)).await;
    assert_eq!(dut.y.get(), 0);
}
