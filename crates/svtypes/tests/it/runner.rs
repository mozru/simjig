//! The Verilator runner, against the installed Verilator (from `pixi run`).

use std::path::PathBuf;

use svtypes::error::Error;
use svtypes::verilator::{JsonPassOpts, json_pass};

use crate::Case;

/// The installed Verilator still produces the IR of the committed fixtures. A failure
/// here after a Verilator upgrade means: run the regen test and review the diffs.
#[test]
fn json_pass_matches_committed_fixtures() {
    for case in Case::all() {
        let live = json_pass(&case.opts()).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        assert_eq!(live, case.committed().unwrap(), "{}", case.name);
    }
}

#[test]
fn param_overrides_reach_the_ir() {
    let case = Case::get("params_override");
    let mut opts = case.opts();
    opts.params = vec![("W".into(), "5".into())];
    let design = json_pass(&opts).unwrap();
    let top = design.top_module();
    let w = top.params.iter().find(|p| p.name == "W").unwrap();
    assert_eq!(
        format!("{:?}", w.value),
        "Bits(Literal { width: 32, signed: true, words: [5] })"
    );
    let a = top.ports.iter().find(|p| p.name == "a").unwrap();
    assert_eq!(design.ty(a.ty).width, Some(5));
}

#[test]
fn verilator_errors_carry_command_and_stderr() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/structs");
    let opts = JsonPassOpts {
        sources: vec![dir.join("design.sv")],
        top: "no_such_module".into(),
        ..JsonPassOpts::default()
    };
    let err = json_pass(&opts).unwrap_err();
    let Error::Tool {
        command,
        status,
        stderr,
    } = &err
    else {
        panic!("expected a tool error, got {err}");
    };
    assert!(command.starts_with("verilator --json-only"), "{command}");
    assert!(
        command.contains("--no-timing --top-module no_such_module"),
        "{command}"
    );
    assert!(status.is_some_and(|s| s != 0), "{status:?}");
    assert!(stderr.contains("no_such_module"), "{stderr}");
}

/// X/Z constants are recorded as unrepresentable, with the reason, instead of failing
/// the whole design; the two-state ones beside them decode as usual.
#[test]
fn four_state_constants_are_recorded_not_fatal() {
    let rtl = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/rtl");
    let design = json_pass(&JsonPassOpts {
        sources: vec![rtl.join("xz_params.sv")],
        top: "xz_params".into(),
        ..JsonPassOpts::default()
    })
    .unwrap();
    let pkg = design.packages.iter().find(|p| p.name == "xz_pkg").unwrap();
    let names = |ps: &[svtypes::ir::Param]| ps.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&pkg.params), ["OK"]);
    let skipped: Vec<_> = pkg
        .unrepresentable
        .iter()
        .map(|u| (u.name.as_str(), u.value.as_str()))
        .collect();
    assert_eq!(skipped, [("XV", "4'b10x1"), ("ZV", "4'bzz00")]);
    assert!(
        pkg.unrepresentable[0].reason.contains("X or Z"),
        "{:?}",
        pkg.unrepresentable[0]
    );
    let top = design.top_module();
    assert!(top.params.is_empty(), "{:?}", top.params);
    assert_eq!(top.unrepresentable[0].name, "P");
}
