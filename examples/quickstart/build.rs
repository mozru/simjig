//! Verilates each design, generates its Rust module and C++ glue, and links the model into
//! this crate's tests.

fn main() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("inv")
        .sources(["rtl/inv.sv"])
        .top("inv")
        .build()?;
    simjig_build::Dut::new("flop")
        .sources(["rtl/flop.sv"])
        .top("flop")
        .trace_fst(true) // FST waves, for waves on failure
        .build()?;
    Ok(())
}
