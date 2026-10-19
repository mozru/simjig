//! An optional DUT in a real Cargo build of a user crate (`tests/fixtures/optional-crate`):
//! after `opt` built once and then failed (its failure handled by the build script), an
//! ungated `include_dut!(opt)` fails to compile naming the DUT, where the stale module
//! used to compile and the test failed to link (`Undefined symbols:
//! _simjig_vtable_V<dut>_...`); code gated on `#[cfg(simjig_dut_opt)]` builds either way.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::{copy_dir, dut, fixtures};

/// The fixture crate under `root` (a stable path, so its target dir is reused between
/// runs), pointed at the crates under test, with the inverter as `duts/inv`.
fn prepare(root: &Path) -> PathBuf {
    let krate = root.join("crate");
    copy_dir(&fixtures().join("optional-crate"), &krate);
    copy_dir(&dut("inv"), &krate.join("duts/inv"));
    let this_crate = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = krate.join("Cargo.toml");
    let path = |p: &Path| format!("path = {:?}", p.display().to_string());
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace(
            r#"path = "../../../../simjig""#,
            &path(&this_crate.parent().unwrap().join("simjig")),
        )
        .replace(r#"path = "../../..""#, &path(this_crate));
    std::fs::write(&manifest, text).unwrap();
    // The workspace lock file: no network, the same dependency versions.
    std::fs::copy(
        this_crate.join("../../Cargo.lock"),
        krate.join("Cargo.lock"),
    )
    .unwrap();
    // A fresh copy must rebuild its build script: `std::fs::copy` keeps the source's
    // mtime on macOS, which can be older than a build script compiled from an older copy.
    std::fs::File::options()
        .append(true)
        .open(krate.join("build.rs"))
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
    krate
}

/// `cargo <args>` in the fixture crate, with `opt` working and its failure handled.
fn command(krate: &Path, target: &Path, args: &[&str]) -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command
        .args(args)
        .current_dir(krate)
        .env("CARGO_TARGET_DIR", target)
        // simjig's own test models are for its repository; the fixture builds none.
        .env("SIMJIG_TEST_MODELS", "0")
        // The tests match Cargo's messages as plain text (`warning: `); an inherited
        // `CARGO_TERM_COLOR=always`, as CI sets, wraps them in ANSI escapes.
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("SIMJIG_CACHE_DIR")
        .env_remove("FIXTURE_BREAK")
        .env_remove("FIXTURE_FATAL")
        .env_remove("FIXTURE_SWEEP");
    command
}

/// `cargo <args>` in the fixture crate, with `opt` broken when `broken`.
fn cargo(krate: &Path, target: &Path, args: &[&str], broken: bool) -> Output {
    let mut command = command(krate, target, args);
    if broken {
        command.env("FIXTURE_BREAK", "1");
    }
    command.output().unwrap()
}

/// The prepared fixture crate and its target dir, with the lock that serialises the tests.
fn fixture() -> (PathBuf, PathBuf, std::fs::File) {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fixture-optional");
    std::fs::create_dir_all(&root).unwrap();
    let lock = std::fs::File::create(root.join("lock")).unwrap();
    lock.lock().unwrap();
    let krate = prepare(&root);
    (krate, root.join("target"), lock)
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_failed_optional_dut_is_a_compile_error_or_gated_off() {
    let (krate, target, _lock) = fixture();
    let test = ["test", "--offline"];

    let good = cargo(&krate, &target, &test, false);
    let out = text(&good);
    assert!(good.status.success(), "{out}");
    for ran in ["test opt_inverts ... ok", "test opt_built ... ok"] {
        assert!(out.contains(ran), "{ran}:\n{out}");
    }

    let gated = cargo(
        &krate,
        &target,
        &["test", "--offline", "--test", "gated"],
        true,
    );
    let out = text(&gated);
    assert!(gated.status.success(), "{out}");
    // The cause is warned once, by `Dut::build`; the script's own warning only says what
    // it skipped.
    let warned = |what: &str| {
        out.lines()
            .filter(|l| l.starts_with("warning: ") && l.contains(what))
            .count()
    };
    assert_eq!(warned("simjig: elaborating DUT `opt` failed"), 1, "{out}");
    assert_eq!(warned("Cannot find file"), 1, "{out}");
    assert!(out.contains("optional DUT `opt` skipped"), "{out}");
    assert!(out.contains("test opt_skipped ... ok"), "{out}");
    assert!(!out.contains("opt_built"), "{out}");

    let ungated = cargo(
        &krate,
        &target,
        &["test", "--offline", "--test", "ungated"],
        true,
    );
    let out = text(&ungated);
    assert!(!ungated.status.success(), "{out}");
    assert!(out.contains("DUT `opt` failed to build"), "{out}");
    assert!(out.contains("#[cfg(simjig_dut_opt)]"), "{out}");
    for link_error in ["Undefined symbols", "undefined reference", "linking with"] {
        assert!(!out.contains(link_error), "{link_error}:\n{out}");
    }

    // `cargo check` sees the same stub.
    let check = cargo(&krate, &target, &["check", "--offline", "--tests"], true);
    let out = text(&check);
    assert!(!check.status.success(), "{out}");
    assert!(out.contains("DUT `opt` failed to build"), "{out}");

    // Fixed again, the stub is replaced and the gate is set again.
    let fixed = cargo(&krate, &target, &test, false);
    let out = text(&fixed);
    assert!(fixed.status.success(), "{out}");
    for ran in ["test opt_inverts ... ok", "test opt_built ... ok"] {
        assert!(out.contains(ran), "{ran}:\n{out}");
    }
    assert!(!out.contains("opt_skipped"), "{out}");
}

/// Whether `line` is one of Cargo's progress lines, which come before anything a build
/// script printed.
fn is_progress(line: &str) -> bool {
    let line = line.trim_start();
    line.is_empty()
        || [
            "Compiling ",
            "Blocking ",
            "Locking ",
            "Adding ",
            "Checking ",
        ]
        .iter()
        .any(|p| line.starts_with(p))
}

/// A script that returns a failed DUT's error: Cargo prints its warnings, then dumps its
/// whole stdout (every `rerun-if` line, which a handled failure needs and a script cannot
/// hold back until it knows its exit status), and last its stderr, the error. The cause
/// leads anyway: the first line after Cargo's progress is `Dut::build`'s warning of the
/// headline, above the dump.
#[test]
fn a_failed_scripts_output_leads_with_the_cause() {
    let (krate, target, _lock) = fixture();
    let out = command(&krate, &target, &["build", "--offline", "--tests"])
        .env("FIXTURE_BREAK", "1")
        .env("FIXTURE_FATAL", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    let first = stderr.lines().find(|l| !is_progress(l)).unwrap_or_default();
    let missing = krate.join("duts/inv/missing.sv");
    assert_eq!(
        first,
        format!(
            "warning: simjig-fixture-optional@0.0.0: simjig: elaborating DUT `opt` failed \
             (exit status: 1): Cannot find file containing module: '{}'",
            missing.display()
        ),
        "{stderr}"
    );
    let dump = stderr.find("--- stdout").expect(&stderr);
    assert!(stderr.find(first).unwrap() < dump, "{stderr}");
}

/// A failed `Sweep::build` leads with its cause as `Dut::build` does, warned once: the
/// sweep's own error (no members) by the sweep, a member's error by the member alone.
#[test]
fn a_failed_sweep_warns_its_cause_once() {
    let (krate, target, _lock) = fixture();
    for (kind, cause) in [
        ("empty", "a sweep needs at least one DUT"),
        ("member", "Cannot find file containing module"),
    ] {
        let out = command(&krate, &target, &["build", "--offline", "--tests"])
            .env("FIXTURE_SWEEP", kind)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{stderr}");
        let first = stderr.lines().find(|l| !is_progress(l)).unwrap_or_default();
        assert!(
            first.starts_with("warning: simjig-fixture-optional@0.0.0: simjig: ")
                && first.contains(cause),
            "{kind}: {stderr}"
        );
        let warned = stderr
            .lines()
            .filter(|l| l.starts_with("warning: ") && l.contains(cause))
            .count();
        assert_eq!(warned, 1, "{kind}: {stderr}");
    }
}
