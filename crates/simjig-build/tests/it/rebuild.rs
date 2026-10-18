//! What a rerun of a real build script compiles again, and what makes Cargo rerun it (see
//! `tests/fixtures/rebuild-crate`). The tests take turns through a file lock on one copy
//! of the crate, whose target dir is kept between runs.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::fixture_crate::cargo_with;
use crate::support::{copy_dir, dut, fixtures, write_script};

/// The fixture crate under `root` (a stable path), pointed at this crate, with its DUTs.
fn prepare(root: &Path) -> PathBuf {
    let krate = root.join("crate");
    copy_dir(&fixtures().join("rebuild-crate"), &krate);
    for d in ["counter", "inv", "flop"] {
        copy_dir(&dut(d), &krate.join("duts").join(d));
    }
    let this_crate = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = krate.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap().replace(
        r#"path = "../../..""#,
        &format!("path = {:?}", this_crate.display().to_string()),
    );
    std::fs::write(&manifest, text).unwrap();
    std::fs::copy(
        this_crate.join("../../Cargo.lock"),
        krate.join("Cargo.lock"),
    )
    .unwrap();
    // A fresh copy must rebuild its build script, whatever the copy did to the mtime.
    touch(&krate.join("build.rs"));
    krate
}

/// The prepared crate and its target dir, with the lock that serialises the tests.
fn fixture() -> (PathBuf, PathBuf, std::fs::File) {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fixture-rebuild");
    std::fs::create_dir_all(&root).unwrap();
    let lock = std::fs::File::create(root.join("lock")).unwrap();
    lock.lock().unwrap();
    let krate = prepare(&root);
    let target = root.join("target");
    (krate, target, lock)
}

fn touch(path: &Path) {
    std::fs::File::options()
        .append(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
}

/// `cargo build -v`; returns whether the build script ran.
fn build(krate: &Path, target: &Path, envs: &[(&str, OsString)]) -> bool {
    let (_, stderr) = cargo_with(krate, target, &["build", "--offline", "-v"], envs);
    stderr.contains("build-script-build`")
}

/// The build script's `OUT_DIR`s (one per build-script fingerprint).
fn out_dirs(target: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(target.join("debug/build")) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|d| {
            d.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("simjig-fixture-rebuild-"))
        })
        .map(|d| d.join("out"))
        .filter(|d| d.is_dir())
        .collect()
}

fn model_archives(cache: &Path) -> usize {
    std::fs::read_dir(cache.join("archives")).unwrap().count()
}

/// Editing one DUT compiles only that DUT's model again, and a rerun with no change
/// compiles nothing, also on the first rerun after a cold build. Each model archive's key
/// once listed `OUT_DIR/simjig/`, which holds every DUT's generated files (and the shims
/// of earlier configurations), so it changed whenever any DUT did, and while the cold
/// build added the DUTs one by one.
#[test]
fn a_rerun_compiles_only_the_edited_model() {
    let (krate, target, _lock) = fixture();
    // A cold build: no cache, and no `OUT_DIR` left by an earlier run.
    let cache = krate.parent().unwrap().join("cache");
    for dir in out_dirs(&target).into_iter().chain([cache.clone()]) {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
    let envs = [
        ("SIMJIG_CACHE_DIR", cache.clone().into_os_string()),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    assert!(build(&krate, &target, &envs));
    assert_eq!(model_archives(&cache), 3, "one archive per DUT");

    let inv = krate.join("duts/inv/inv.sv");
    touch(&inv);
    assert!(
        build(&krate, &target, &envs),
        "a touched source reruns the script"
    );
    assert_eq!(model_archives(&cache), 3, "no change compiles nothing");

    let script = krate.join("build.rs");
    let text = std::fs::read_to_string(&script).unwrap();
    let edited = text.replace(r#".param("WIDTH", 12)"#, r#".param("WIDTH", 13)"#);
    assert_ne!(text, edited);
    std::fs::write(&script, edited).unwrap();
    assert!(build(&krate, &target, &envs));
    assert_eq!(
        model_archives(&cache),
        4,
        "only the counter is compiled again"
    );

    touch(&inv);
    assert!(build(&krate, &target, &envs));
    assert_eq!(model_archives(&cache), 4, "no change compiles nothing");
}
/// An upgrade of the compiler in place (same `CXX`, same `PATH`) reruns the build script:
/// it reruns on the executable `CXX` resolves to, here a script behind a symlink found on
/// `PATH`.
#[test]
fn a_compiler_changed_in_place_reruns_the_build_script() {
    let (krate, target, _lock) = fixture();
    let root = krate.parent().unwrap();
    let name = std::env::var("CXX").unwrap_or_else(|_| "c++".to_owned());
    let path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&path)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(&name));
    let (bin, tools) = (root.join("bin"), root.join("tools"));
    for d in [&bin, &tools] {
        std::fs::create_dir_all(d).unwrap();
    }
    let compiler = tools.join("cxx");
    write_script(
        &compiler,
        &format!("#!/bin/sh\nexec '{}' \"$@\"\n", real.display()),
    );
    let link = bin.join("simjig-test-cxx");
    if link.symlink_metadata().is_err() {
        std::os::unix::fs::symlink(&compiler, &link).unwrap();
    }
    let envs = [
        (
            "PATH",
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap(),
        ),
        ("CXX", "simjig-test-cxx".into()),
        ("SIMJIG_CACHE_DIR", root.join("cache").into_os_string()),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    build(&krate, &target, &envs);
    assert!(!build(&krate, &target, &envs), "nothing changed");
    let text = std::fs::read_to_string(&compiler).unwrap();
    std::fs::write(&compiler, format!("{text}# upgraded\n")).unwrap();
    assert!(
        build(&krate, &target, &envs),
        "the compiler changed, but the build script did not rerun"
    );
}
