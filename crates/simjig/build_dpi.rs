//! The DPI test models (`tests/dpi_tests`), built through the real pipeline
//! (`simjig_build::Dut`): JSON pass, code generation (including the DPI trait and
//! trampolines), verilation, the `__Dpi.h` cross-check and compilation.
//!
//! - `dpi`: `tests/duts/dpi`, two imports (`dpi_add`, `dpi_tick`);
//! - `dpi_types`: `tests/dpi_tests/dpi_types.sv`, every DPI type simjig implements.
//!
//! Like the rest of `build.rs`, link directives are narrowed to test binaries
//! (`rustc-link-arg-tests`), so the library and its users never link these models. The
//! emitter appends the `dut_model!` glue when the code generator does not emit it yet.

use std::fmt::Write as _;
use std::path::PathBuf;

use simjig_build::svtypes::ir::Design;
use simjig_build::{BuildEnv, Dut, DutConfig, Emitted, Error, codegen_emitter};

/// Builds both models; returns the `cargo::` lines to print.
pub(crate) fn build() -> Result<Vec<String>, String> {
    let env = BuildEnv::from_cargo().map_err(|e| e.to_string())?;
    let duts = [
        Dut::new("dpi")
            .sources(["../../tests/duts/dpi/dpi.sv"])
            .top("dpi"),
        Dut::new("dpi_types")
            .sources(["tests/dpi_tests/dpi_types.sv"])
            .top("dpi_types"),
    ];
    let mut lines = Vec::new();
    for dut in duts {
        let built = dut
            .emitter(emit)
            .build_in(&env)
            .map_err(|e| e.to_string())?;
        lines.extend(tests_only(&built.directives));
    }
    Ok(lines)
}

/// The code generator, plus `dut_model!` if it did not emit one.
fn emit(design: &Design, config: &DutConfig) -> Result<Emitted, Error> {
    let mut emitted = codegen_emitter(design, config)?;
    if let Some(rust) = emitted.rust.as_mut()
        && !rust.contains("dut_model!")
    {
        let prefix = config.prefix();
        writeln!(
            rust,
            "\n::simjig::__private::dut_model!(\n    Dut,\n    prefix = {prefix:?},\n    \
             symbol = \"simjig_vtable_{prefix}\",\n    precision_exponent = {},\n);",
            design.time_precision.0
        )
        .map_err(|e| Error::Emit {
            dut: config.name.clone(),
            msg: e.to_string(),
        })?;
    }
    Ok(emitted)
}

/// simjig-build's directives with every link directive narrowed to test binaries: a
/// static library becomes its archive's path, a plain library `-L<dirs> -l<name>`, and a
/// link argument its `-tests` form.
fn tests_only(directives: &[String]) -> Vec<String> {
    let mut search: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for d in directives {
        let rest = d.strip_prefix("cargo::").unwrap_or(d);
        if let Some(dir) = rest.strip_prefix("rustc-link-search=native=") {
            search.push(PathBuf::from(dir));
        } else if let Some(name) = rest.strip_prefix("rustc-link-lib=static=") {
            let file = format!("lib{name}.a");
            let archive = search
                .iter()
                .rev()
                .map(|dir| dir.join(&file))
                .find(|p| p.exists())
                .unwrap_or_else(|| PathBuf::from(&file));
            out.push(format!("cargo::rustc-link-arg-tests={}", archive.display()));
        } else if let Some(name) = rest.strip_prefix("rustc-link-lib=") {
            for dir in &search {
                out.push(format!("cargo::rustc-link-arg-tests=-L{}", dir.display()));
            }
            out.push(format!("cargo::rustc-link-arg-tests=-l{name}"));
        } else if let Some(arg) = rest.strip_prefix("rustc-link-arg=") {
            out.push(format!("cargo::rustc-link-arg-tests={arg}"));
        } else {
            out.push(d.clone());
        }
    }
    out
}
