//! Compile-time errors of `#[simjig::test]`, checked against `tests/harness_ui/*.stderr`
//! (regenerate with `TRYBUILD=overwrite`, then review the diff).
//!
//! trybuild compiles the cases with a nested cargo in `<target>/tests/trybuild`, which
//! still reads the repository's `.cargo/config.toml` (`SIMJIG_TEST_MODELS=1`). simjig's
//! build script must not verilate its test models there: none of the cases links them, and
//! on a cold target that build alone can outlast nextest's time limit.

use std::path::{Path, PathBuf};

/// The line simjig's build script prints once it built the test models.
const MODELS_BUILT: &str = "cargo::rustc-cfg=simjig_test_models";

/// The entries of `dir`; none if it does not exist (a cold target).
fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

/// The build-script run directories under the target directory `target` whose output says
/// the models were built: `<target>/debug/build/*` and, as trybuild passes `--target`,
/// `<target>/<triple>/debug/build/*`.
fn model_builds(target: &Path) -> Vec<PathBuf> {
    entries(target)
        .iter()
        .flat_map(|d| [d.join("build"), d.join("debug/build")])
        .flat_map(|build| entries(&build))
        .filter(|dir| {
            std::fs::read_to_string(dir.join("output"))
                .is_ok_and(|out| out.lines().any(|l| l == MODELS_BUILT))
        })
        .collect()
}

#[test]
fn signature_errors_point_at_the_offending_tokens() {
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("CARGO_TARGET_TMPDIR is <target>/tmp")
        .join("tests/trybuild");
    // Removing a run directory makes cargo rerun that build script, so a leftover from an
    // earlier build cannot hide what this one does.
    for dir in model_builds(&target) {
        std::fs::remove_dir_all(&dir).expect("removable");
    }
    // trybuild runs the cases when `TestCases` is dropped, at the end of this statement.
    trybuild::TestCases::new().compile_fail("tests/harness_ui/*.rs");
    assert_eq!(
        model_builds(&target),
        Vec::<PathBuf>::new(),
        "the nested trybuild build verilated simjig's test models"
    );
}
