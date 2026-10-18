//! Builds the optional DUT `opt`, from a source that does not exist when `FIXTURE_BREAK`
//! is set, and handles its failure the way a script with an optional model does; with
//! `FIXTURE_FATAL` set too, it returns the error instead. With `FIXTURE_SWEEP=empty` it
//! instead builds a sweep with no members, and with `FIXTURE_SWEEP=member` one whose
//! member's source does not exist, and returns the error.

fn main() -> Result<(), simjig_build::Error> {
    println!("cargo::rerun-if-env-changed=FIXTURE_BREAK");
    println!("cargo::rerun-if-env-changed=FIXTURE_FATAL");
    println!("cargo::rerun-if-env-changed=FIXTURE_SWEEP");
    if let Some(kind) = std::env::var_os("FIXTURE_SWEEP") {
        let sweep = simjig_build::Sweep::new("opt_sweep");
        let sweep = match kind.to_str() {
            Some("member") => sweep.dut(
                simjig_build::Dut::new("swept")
                    .sources(["duts/inv/missing.sv"])
                    .top("inv"),
            ),
            _ => sweep,
        };
        sweep.build()?;
        return Ok(());
    }
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
