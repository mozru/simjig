//! Snapshots of the IR decoded from the committed Verilator 5.052 fixture output, and the
//! regeneration of that output.

use std::fs;

use serde_json::Value;
use svtypes::verilator::write_json_files;

use crate::Case;

#[test]
fn ir_snapshots() {
    for case in Case::all() {
        let design = case
            .committed()
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        let json = serde_json::to_string_pretty(&design).unwrap();
        insta::assert_snapshot!(case.name.as_str(), json);
    }
}

#[test]
fn ir_round_trips_through_json() {
    for case in Case::all() {
        let design = case.committed().unwrap();
        let json = serde_json::to_string(&design).unwrap();
        let back: svtypes::ir::Design = serde_json::from_str(&json).unwrap();
        assert_eq!(back, design, "{}", case.name);
    }
}

/// Rewrites `tree.json`, `tree.meta.json` and `verilator-version` of every fixture with
/// the installed Verilator. Run with `pixi run cargo nextest run -p svtypes --run-ignored
/// only regen_fixtures`, then review the diff and the snapshot changes.
#[test]
#[ignore = "rewrites the committed fixtures; run by hand after a Verilator upgrade"]
fn regen_fixtures() {
    for case in Case::all() {
        let out = case.dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let files =
            write_json_files(&case.opts(), &out).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        fs::copy(&files.tree, case.dir.join("tree.json")).unwrap();
        let meta: Value = serde_json::from_str(&fs::read_to_string(&files.meta).unwrap()).unwrap();
        let meta = serde_json::to_string_pretty(&normalize_meta(meta)).unwrap();
        fs::write(case.dir.join("tree.meta.json"), meta + "\n").unwrap();
        fs::write(case.dir.join("verilator-version"), files.version + "\n").unwrap();
        fs::remove_dir_all(&out).unwrap();
    }
}

/// Makes `.tree.meta.json` reproducible: drops the `pointers` map (heap addresses) and
/// reduces every path to its file name (absolute paths differ between machines).
fn normalize_meta(mut meta: Value) -> Value {
    let obj = meta.as_object_mut().unwrap();
    obj.remove("pointers");
    if let Some(Value::Array(names)) = obj.get_mut("ptrFieldNames") {
        // Printed from a hash set: the order is not stable.
        names.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    }
    if let Some(Value::Object(files)) = obj.get_mut("files") {
        for file in files.values_mut() {
            for key in ["filename", "realpath"] {
                if let Some(Value::String(path)) = file.get_mut(key)
                    && let Some(base) = path.rsplit('/').next()
                {
                    *path = base.to_owned();
                }
            }
        }
    }
    meta
}
