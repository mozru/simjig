//! A real Cargo build of a user crate whose `build.rs` uses `simjig-build` on the shared
//! DUTs, then its tests, which call the models' vtables (see
//! `tests/fixtures/vtable-crate`). Each test here runs one fixture test; nextest runs them
//! in separate processes, so they take turns through a file lock, and the first one
//! builds the crate for the others.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::support::{copy_dir, dut, fixtures};

/// Copies the fixture crate and the DUTs it uses under `root` (a stable path, so its own
/// target dir is reused between runs) and points it at this crate.
fn prepare(root: &Path) -> PathBuf {
    let krate = root.join("crate");
    copy_dir(&fixtures().join("vtable-crate"), &krate);
    for d in ["counter", "timing_dut", "inv", "flop"] {
        copy_dir(&dut(d), &krate.join("duts").join(d));
    }
    let this_crate = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::copy(
        this_crate.join("tests/codegen_it/stub.rs"),
        krate.join("src/stub.rs"),
    )
    .unwrap();
    let manifest = krate.join("Cargo.toml");
    let bits = this_crate.parent().unwrap().join("simjig-bits");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace(
            r#"path = "../../..""#,
            &format!("path = {:?}", this_crate.display().to_string()),
        )
        .replace(
            r#"path = "../../../../simjig-bits""#,
            &format!("path = {:?}", bits.display().to_string()),
        );
    std::fs::write(&manifest, text).unwrap();
    // Start from the workspace lock file so the build needs no network and uses
    // the same dependency versions.
    let lock = this_crate.join("../../Cargo.lock");
    std::fs::copy(lock, krate.join("Cargo.lock")).unwrap();
    krate
}

/// The prepared fixture crate and its target dir, with the lock that serialises its users.
fn fixture() -> (PathBuf, PathBuf, std::fs::File) {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fixture-vtable");
    std::fs::create_dir_all(&root).unwrap();
    let lock = std::fs::File::create(root.join("lock")).unwrap();
    lock.lock().unwrap();
    let krate = prepare(&root);
    let target = krate.parent().unwrap().join("target");
    (krate, target, lock)
}

/// Runs `cargo <args>` in the fixture crate; returns stdout and stderr after checking
/// that it succeeded.
fn cargo(krate: &Path, target: &Path, args: &[&str]) -> (String, String) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(args)
        .current_dir(krate)
        .env("CARGO_TARGET_DIR", target)
        .env_remove("SIMJIG_CACHE_DIR")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "cargo {args:?} failed:\n{stdout}\n{stderr}"
    );
    (stdout, stderr)
}

/// Runs one test of the fixture crate and checks that it ran and passed.
fn run_fixture_test(name: &str) {
    let (krate, target, _lock) = fixture();
    let (stdout, _) = cargo(
        &krate,
        &target,
        &["test", "--offline", "--quiet", "--", "--exact", name],
    );
    assert!(stdout.contains("1 passed"), "{name} did not run:\n{stdout}");
    // The cache was derived from OUT_DIR: it sits in the fixture's target dir.
    assert!(target.join("simjig-cache/models").is_dir());
}

#[test]
fn counter_vtable_end_to_end() {
    run_fixture_test("counter_vtable_end_to_end");
}

#[test]
fn timing_model_schedules_its_own_events() {
    run_fixture_test("timing_model_schedules_its_own_events");
}

#[test]
fn fst_tracing_links_and_writes_a_trace() {
    run_fixture_test("fst_tracing_links_and_writes_a_trace");
}

#[test]
fn two_models_share_one_binary() {
    run_fixture_test("two_models_share_one_binary");
}

#[test]
fn integration_test_without_the_lib_links_the_model() {
    run_fixture_test("standalone_test_links_the_model");
}

/// After a build that wrote new generated inputs into the cache (here: all of them, after
/// removing `inputs/` and touching a source), the next build is a no-op: the build script
/// does not run again. The inputs are content-addressed and derive from tracked
/// configuration, so they are no reason to rerun; listing them made the first no-op build
/// after every new DUT configuration rerun the whole script (their mtime is later than the
/// script's start).
#[test]
fn a_no_op_build_after_new_generated_inputs_is_fresh() {
    let (krate, target, _lock) = fixture();
    let build = ["build", "--offline", "--tests", "-v"];
    cargo(&krate, &target, &build);
    let inputs = target.join("simjig-cache/inputs");
    if inputs.exists() {
        std::fs::remove_dir_all(&inputs).unwrap();
    }
    let source = krate.join("duts/inv/inv.sv");
    std::fs::File::options()
        .append(true)
        .open(&source)
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
    let (_, stderr) = cargo(&krate, &target, &build);
    assert!(
        stderr.contains("build-script-build`") && inputs.is_dir(),
        "a touched source reruns the script, which stores the inputs again:\n{stderr}"
    );
    let (_, stderr) = cargo(&krate, &target, &build);
    assert!(
        stderr.contains("Fresh simjig-fixture-vtable") && !stderr.contains("build-script-build`"),
        "the second build reran the build script:\n{stderr}"
    );
}
