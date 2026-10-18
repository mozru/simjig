//! Integration tests: the fixture corpus, the Verilator runner and strict decoding.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns; a panic is the failure report"
)]

mod cli;
mod dpi;
mod fixtures;
mod header;
mod runner;
mod strict;

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use svtypes::ir::Design;
use svtypes::verilator::JsonPassOpts;

/// `tests/fixtures/<case>/case.json`: how the case is elaborated.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CaseOpts {
    top: String,
    params: Vec<(String, String)>,
    defines: Vec<(String, Option<String>)>,
    extra_args: Vec<String>,
}

/// One fixture directory.
#[derive(Debug)]
struct Case {
    name: String,
    dir: PathBuf,
}

impl Case {
    fn all() -> Vec<Case> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut cases: Vec<Case> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("design.sv").exists())
            .map(|dir| Case {
                name: dir.file_name().unwrap().to_string_lossy().into_owned(),
                dir,
            })
            .collect();
        cases.sort_by(|a, b| a.name.cmp(&b.name));
        assert!(cases.len() >= 10, "fixture corpus is missing cases");
        cases
    }

    fn get(name: &str) -> Case {
        Case::all().into_iter().find(|c| c.name == name).unwrap()
    }

    fn opts(&self) -> JsonPassOpts {
        let text = fs::read_to_string(self.dir.join("case.json")).unwrap();
        let case: CaseOpts = serde_json::from_str(&text).unwrap();
        JsonPassOpts {
            sources: vec![self.dir.join("design.sv")],
            top: case.top,
            defines: case.defines,
            params: case.params,
            extra_args: case.extra_args,
            ..JsonPassOpts::default()
        }
    }

    fn read(&self, file: &str) -> String {
        fs::read_to_string(self.dir.join(file))
            .unwrap_or_else(|e| panic!("{}/{file}: {e} (run the regen test)", self.name))
    }

    /// The committed Verilator output, decoded.
    fn committed(&self) -> svtypes::error::Result<Design> {
        svtypes::frontend::parse(
            &self.read("tree.json"),
            &self.read("tree.meta.json"),
            &self.read("verilator-version"),
        )
    }
}
