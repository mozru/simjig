//! A real Cargo build of a user crate whose `build.rs` uses `simjig-build`, then a run of
//! the resulting binary, which drives the linked Verilator model.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::support::{copy_dir, fixtures};

/// Copies the fixture crate under the Cargo target tmp dir (a stable path, so its own
/// target dir is reused between test runs) and points it at this crate.
fn prepare() -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fixture-counter");
    let krate = root.join("crate");
    copy_dir(&fixtures().join("counter-crate"), &krate);
    copy_dir(&fixtures().join("rtl"), &krate.join("rtl"));
    let manifest = krate.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let this_crate = env!("CARGO_MANIFEST_DIR");
    std::fs::write(
        &manifest,
        text.replace(r#"path = "../../..""#, &format!("path = {this_crate:?}")),
    )
    .unwrap();
    // Start from the workspace lock file so the build needs no network and uses the
    // same dependency versions.
    let lock = Path::new(this_crate).join("../../Cargo.lock");
    if !krate.join("Cargo.lock").exists() {
        std::fs::copy(lock, krate.join("Cargo.lock")).unwrap();
    }
    krate
}

#[test]
fn user_crate_builds_links_and_runs_the_model() {
    let krate = prepare();
    let target = krate.parent().unwrap().join("target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["run", "--offline", "--quiet", "--", "5000"])
        .current_dir(&krate)
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("SIMJIG_CACHE_DIR")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "cargo run failed:\n{stderr}");
    // WIDTH=12 wraps at 4096: 5000 mod 4096 = 904.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "904",
        "{stderr}"
    );
    // The cache was derived from OUT_DIR: it sits in the fixture's target dir.
    assert!(target.join("simjig-cache/models").is_dir());
}
