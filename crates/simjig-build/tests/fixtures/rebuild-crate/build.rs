//! Three DUTs, built with the real code generator. The tests edit the counter's `WIDTH`.

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
    Ok(())
}
