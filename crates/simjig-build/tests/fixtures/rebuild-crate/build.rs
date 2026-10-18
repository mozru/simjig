//! Three DUTs, built with the real code generator, and an optional one whose failure the
//! script handles. The tests edit the counter's `WIDTH`.

use simjig_build::{Dut, Error};

fn main() -> Result<(), Error> {
    let dut = |name: &str| {
        Dut::new(name)
            .sources([format!("duts/{name}/{name}.sv")])
            .top(name)
    };
    dut("counter").param("WIDTH", 12).build()?;
    dut("inv").build()?;
    dut("flop").build()?;
    // No sources: a configuration error, found before anything is built.
    // `Dut::build` printed the cause as a warning already.
    if Dut::new("optional").top("optional").build().is_err() {
        println!("cargo::warning=optional DUT skipped");
    }
    Ok(())
}
