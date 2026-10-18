//! The DPI-C emitter: snapshots of the generated trait, `set_dpi` and trampolines; errors
//! for what simjig cannot implement; and the prototype cross-check against the
//! `V<prefix>__Dpi.h` the pinned Verilator writes.
//!
//! The generated code is compiled and run against real models in simjig's `dpi_tests`,
//! which also owns the all-types design used here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use simjig_build::codegen::{CodegenConfig, check_dpi_header, dpi_prototypes, emit_dpi, lower};
use svtypes::ir::{Design, DpiType};
use svtypes::verilator::{JsonPassOpts, json_pass};

use crate::cases;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `tests/duts/dpi` and simjig's all-types DPI design.
fn designs() -> [(&'static str, PathBuf); 2] {
    [
        ("dpi", repo().join("tests/duts/dpi/dpi.sv")),
        (
            "dpi_types",
            repo().join("crates/simjig/tests/dpi_tests/dpi_types.sv"),
        ),
    ]
}

fn ir(top: &str, sv: &Path) -> Design {
    json_pass(&JsonPassOpts {
        sources: vec![sv.to_path_buf()],
        top: top.into(),
        ..JsonPassOpts::default()
    })
    .unwrap()
}

fn emit(design: &Design) -> Result<String, simjig_build::codegen::Error> {
    let lowered = lower(design, &CodegenConfig::default()).unwrap();
    emit_dpi(design, &lowered, "Vprefix")
}

#[test]
fn emitted_dpi_snapshots() {
    for (top, sv) in designs() {
        let text = emit(&ir(top, &sv)).unwrap();
        insta::assert_snapshot!(format!("{top}__dpi"), text);
    }
}

#[test]
fn a_design_without_imports_emits_nothing() {
    let case = cases::fixture("structs");
    assert_eq!(emit(&case.design).unwrap(), "");
}

/// The `dpi` svtypes fixture with only the import `name`.
fn only(name: &str) -> Design {
    let mut design = cases::fixture("dpi").design;
    design.dpi_imports.retain(|i| i.name == name);
    design
}

#[test]
fn unsupported_imports_are_errors_naming_the_import() {
    let mut errors = Vec::new();
    for name in ["f_unsupported", "f_vectors"] {
        errors.push(emit(&only(name)).unwrap_err().to_string());
    }
    let mut logic_result = only("f_scalars");
    logic_result.dpi_imports[0].result = Some(DpiType::LogicVector {
        width: 8,
        signed: false,
    });
    errors.push(emit(&logic_result).unwrap_err().to_string());
    let mut self_named = only("f_void");
    self_named.dpi_imports[0].c_name = "self".into();
    errors.push(emit(&self_named).unwrap_err().to_string());
    insta::assert_snapshot!(errors.join("\n"));
}

/// Runs `verilator --cc` on `sv` into a scratch directory; returns `V<prefix>__Dpi.h`.
fn dpi_header(top: &str, sv: &Path) -> String {
    let dir = cases::scratch(&format!("dpi_header_{top}"));
    let out = Command::new("verilator")
        .args([
            "--cc",
            "--no-timing",
            "-Wno-fatal",
            "--prefix",
            "Vp",
            "--Mdir",
        ])
        .arg(&dir)
        .arg(sv)
        .output()
        .expect("verilator on PATH (run under `pixi run`)");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::read_to_string(dir.join("Vp__Dpi.h")).unwrap()
}

#[test]
fn trampoline_prototypes_match_the_verilator_header() {
    for (top, sv) in designs() {
        let design = ir(top, &sv);
        let header = dpi_header(top, &sv);
        check_dpi_header(&design.dpi_imports, &header).unwrap_or_else(|m| panic!("{top}: {m:?}"));
        // Every import is checked: the header declares exactly the imports.
        let declared = header.matches("// DPI import at").count();
        assert_eq!(declared, dpi_prototypes(&design.dpi_imports).len(), "{top}");
    }
}

#[test]
fn the_cross_check_names_a_drifted_prototype() {
    let (top, sv) = &designs()[1];
    let design = ir(top, sv);
    let header = dpi_header(top, sv);
    let drifted = header.replace("long long* sum", "int* sum");
    let mismatch = check_dpi_header(&design.dpi_imports, &drifted).unwrap_err();
    assert!(mismatch.line > 0);
    insta::assert_snapshot!(mismatch.message);
    let missing = header.replace("extern int ty_c_alias(int x);", "");
    let mismatch = check_dpi_header(&design.dpi_imports, &missing).unwrap_err();
    assert_eq!(
        (mismatch.line, mismatch.message.as_str()),
        (0, "DPI-C import `ty_c_alias` is not declared in the header")
    );
}
