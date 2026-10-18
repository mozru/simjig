//! Builds the counter model and a shim generated for its prefix, the way the emitters
//! will generate theirs.

use std::path::PathBuf;

fn main() -> Result<(), simjig_build::Error> {
    let dut = simjig_build::Dut::new("counter")
        .sources(["rtl/counter.sv"])
        .include_dirs(["rtl"])
        .param("WIDTH", 12)
        .top("counter");
    let prefix = dut.config().prefix();
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let shim = out.join("shim.cpp");
    let template = include_str!("shim.cpp.in");
    std::fs::write(&shim, template.replace("@PREFIX@", &prefix)).expect("write shim");
    println!("cargo::rerun-if-changed=shim.cpp.in");
    let recipe = simjig_build::CppRecipe {
        model_sources: vec![shim],
        ..Default::default()
    };
    dut.recipe(recipe).build()?;
    Ok(())
}
