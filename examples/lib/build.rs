//! Verilates each example design, generates its Rust bindings and C++ glue, and links the
//! model into this crate's tests. Each design has its own function, so adding one is a
//! new function and one call in `main`.

fn main() -> Result<(), simjig_build::Error> {
    round_robin_arbiter()?;
    async_fifo()?;
    Ok(())
}

/// The arbiter with five requesters: a count that is not a power of 2, so the pointer's
/// wrap from 4 to 0 is exercised.
fn round_robin_arbiter() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("round_robin_arbiter")
        .sources(["rtl/round_robin_arbiter.sv"])
        .top("round_robin_arbiter")
        .param("N", 5)
        .trace_fst(true)
        .build()?;
    Ok(())
}

/// The dual-clock FIFO, 8 entries of 8 bits: shallow, so random traffic reaches full often.
fn async_fifo() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("async_fifo")
        .sources(["rtl/async_fifo.sv"])
        .top("async_fifo")
        .param("WIDTH", 8)
        .param("DEPTH", 8)
        .trace_fst(true)
        .build()?;
    Ok(())
}
