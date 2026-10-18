//! Strict decoding: an unknown node or field reached from a port is an error that names
//! the JSON path and the Verilator version.

use serde_json::Value;

use crate::Case;

/// Decodes the `structs` fixture after `edit` changed its tree.
fn decode_edited(edit: impl FnOnce(&mut Value)) -> String {
    let case = Case::get("structs");
    let mut tree: Value = serde_json::from_str(&case.read("tree.json")).unwrap();
    edit(&mut tree);
    let tree = serde_json::to_string(&tree).unwrap();
    svtypes::frontend::parse(&tree, &case.read("tree.meta.json"), "5.052")
        .unwrap_err()
        .to_string()
}

/// The type-table node with `name`.
fn type_node<'v>(tree: &'v mut Value, name: &str) -> &'v mut Value {
    tree["miscsp"][0]["typesp"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|t| t["name"] == name)
        .unwrap()
}

#[test]
fn errors_name_path_and_version() {
    let cases = [
        (
            "unknown_field",
            decode_edited(|t| {
                type_node(t, "pkg::inner_t")["isTagged"] = Value::Bool(true);
            }),
        ),
        (
            "unknown_node",
            decode_edited(|t| {
                type_node(t, "pkg::sdata_t")["type"] = "QUEUEDTYPE".into();
            }),
        ),
        (
            "unsupported_keyword",
            decode_edited(|t| {
                let tag = type_node(t, "pkg::inner_t")["membersp"][0]["refDTypep"].clone();
                let types = t["miscsp"][0]["typesp"].as_array_mut().unwrap();
                let basic = types.iter_mut().find(|n| n["addr"] == tag).unwrap();
                basic["keyword"] = "string".into();
            }),
        ),
        (
            "wrong_lsb",
            decode_edited(|t| {
                type_node(t, "pkg::inner_t")["membersp"][0]["lsb"] = "11".into();
            }),
        ),
        (
            "dangling_reference",
            decode_edited(|t| {
                type_node(t, "pkg::outer_t")["membersp"][1]["refDTypep"] = "(NOPE)".into();
            }),
        ),
    ];
    for (name, message) in cases {
        assert!(message.contains("Verilator 5.052 JSON at /"), "{message}");
        insta::assert_snapshot!(name, message);
    }
}

#[test]
fn rejects_unknown_versions() {
    let case = Case::get("structs");
    let err = svtypes::frontend::parse(
        &case.read("tree.json"),
        &case.read("tree.meta.json"),
        "Verilator 5.070",
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "no svtypes front end for `Verilator 5.070`; supported Verilator versions: 5.052"
    );
}
