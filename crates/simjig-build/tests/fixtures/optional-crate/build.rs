//! Builds the optional DUT `opt`, from a source that does not exist when `FIXTURE_BREAK`
//! is set, and handles its failure the way a script with an optional model does.

fn main() {
    println!("cargo::rerun-if-env-changed=FIXTURE_BREAK");
    let source = match std::env::var_os("FIXTURE_BREAK") {
        Some(_) => "duts/inv/missing.sv",
        None => "duts/inv/inv.sv",
    };
    let built = simjig_build::Dut::new("opt")
        .sources([source])
        .top("inv")
        .build();
    if let Err(e) = built {
        let first = e.to_string().lines().next().unwrap_or_default().to_owned();
        println!("cargo::warning=optional DUT `opt` skipped: {first}");
    }
}
