struct Dut;

#[simjig::test]
async fn two_duts(a: Dut, b: Dut) {}

#[simjig::test]
async fn mutable_context(sim: &mut simjig::TestCtx) {}

#[simjig::test]
async fn context_last(dut: Dut, sim: &simjig::TestCtx) {}

#[simjig::test]
async fn generic<D>(dut: D) {}

fn main() {}
