//! DPI-C imports in the IR: deduplication across module clones, and strict decoding of the
//! import declarations. The full decode of every type class is the `dpi` fixture snapshot.

use serde_json::Value;
use svtypes::verilator::{JsonPassOpts, json_pass};

use crate::Case;

/// Decodes the `dpi` fixture after `edit` changed its tree.
fn decode_edited(edit: impl FnOnce(&mut Value)) -> svtypes::error::Result<svtypes::ir::Design> {
    let case = Case::get("dpi");
    let mut tree: Value = serde_json::from_str(&case.read("tree.json")).unwrap();
    edit(&mut tree);
    let tree = serde_json::to_string(&tree).unwrap();
    svtypes::frontend::parse(&tree, &case.read("tree.meta.json"), "5.052")
}

/// The statement list of the top module, which declares the imports.
fn top_stmts(tree: &mut Value) -> &mut Vec<Value> {
    tree["modulesp"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|m| m["origName"] == "top")
        .unwrap()["stmtsp"]
        .as_array_mut()
        .unwrap()
}

fn import_node<'v>(tree: &'v mut Value, name: &str) -> &'v mut Value {
    top_stmts(tree)
        .iter_mut()
        .find(|s| s["name"] == name && s["dpiImport"] == true)
        .unwrap()
}

#[test]
fn an_unknown_field_on_an_import_is_an_error() {
    let err = decode_edited(|t| {
        import_node(t, "f_int")["isPure"] = Value::Bool(true);
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("unknown field `isPure` on FUNC"), "{err}");
    assert!(err.contains("design.sv:15"), "{err}");
}

#[test]
fn one_c_name_with_two_prototypes_is_an_error() {
    let err = decode_edited(|t| {
        let mut copy = import_node(t, "f_int").clone();
        copy["stmtsp"][1]["direction"] = "INPUT".into();
        top_stmts(t).push(copy);
    })
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("DPI-C import `f_int` is declared twice with different signatures"),
        "{err}"
    );
}

/// A parameterised module is cloned per parameter set; its import is listed once.
#[test]
fn imports_of_cloned_modules_are_listed_once() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("dpi_clones");
    std::fs::create_dir_all(&dir).unwrap();
    let sv = dir.join("clones.sv");
    std::fs::write(
        &sv,
        "module sub #(parameter int W = 1) (input logic [W-1:0] a, output int y);\n\
         \x20 import \"DPI-C\" function int twice(input int x);\n\
         \x20 assign y = twice(int'(a));\n\
         endmodule\n\
         module clones (input logic [3:0] a, output int y1, output int y2);\n\
         \x20 sub #(.W(2)) u1 (.a(a[1:0]), .y(y1));\n\
         \x20 sub #(.W(4)) u2 (.a(a), .y(y2));\n\
         endmodule\n",
    )
    .unwrap();
    let design = json_pass(&JsonPassOpts {
        sources: vec![sv],
        top: "clones".into(),
        ..JsonPassOpts::default()
    })
    .unwrap();
    let subs = design
        .modules
        .iter()
        .filter(|m| m.orig_name == "sub")
        .count();
    assert_eq!(subs, 2, "the fixture must clone `sub`");
    let names: Vec<_> = design.dpi_imports.iter().map(|i| &i.c_name).collect();
    assert_eq!(names, ["twice"]);
    assert_eq!(design.dpi_imports[0].loc.as_deref(), Some("clones.sv:2"));
}

#[test]
fn designs_without_dpi_serialize_without_dpi_fields() {
    let design = Case::get("structs").committed().unwrap();
    let json = serde_json::to_string(&design).unwrap();
    assert!(!json.contains("dpi"), "{json}");
}
