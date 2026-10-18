//! Verilates each design, generates its Rust bindings and C++ glue, and links the model
//! into this crate's tests. A test file then writes `simjig::include_dut!(inv);`.

fn main() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("inv")
        .sources(["rtl/inv.sv"])
        .top("inv")
        .build()?;
    simjig_build::Dut::new("flop")
        .sources(["rtl/flop.sv"])
        .top("flop")
        .build()?;
    Ok(())
}
