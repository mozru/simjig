//! Verilates each design, generates its Rust bindings and C++ glue, and links the model
//! into this crate's tests. A test file then writes `simjig::include_dut!(inv);`.

fn main() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("inv")
        .sources(["rtl/inv.sv"])
        .top("inv")
        .build()?;
    // `.trace_fst(true)`: the flop can write FST waves, so its failures get waves from a
    // replay (`SIMJIG_WAVES_ON_FAIL=1`, `cargo simjig waves`). `.timing(true)` and
    // `.coverage(true)` are the other model options.
    simjig_build::Dut::new("flop")
        .sources(["rtl/flop.sv"])
        .top("flop")
        .trace_fst(true)
        .build()?;
    Ok(())
}
