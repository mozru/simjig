//! Builds the optional DUT `opt`, from a source that does not exist when `FIXTURE_BREAK`
//! is set, and handles its failure the way a script with an optional model does; with
//! `FIXTURE_FATAL` set too, it returns the error instead.

fn main() -> Result<(), simjig_build::Error> {
    println!("cargo::rerun-if-env-changed=FIXTURE_BREAK");
    println!("cargo::rerun-if-env-changed=FIXTURE_FATAL");
    let source = match std::env::var_os("FIXTURE_BREAK") {
        Some(_) => "duts/inv/missing.sv",
        None => "duts/inv/inv.sv",
    };
    let built = simjig_build::Dut::new("opt")
        .sources([source])
        .top("inv")
        .build();
    match built {
        Err(e) if std::env::var_os("FIXTURE_FATAL").is_some() => return Err(e),
        // `Dut::build` printed the cause as a warning already.
        Err(_) => println!("cargo::warning=optional DUT `opt` skipped"),
        Ok(_) => {}
    }
    Ok(())
}
