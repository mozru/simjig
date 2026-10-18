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
    krate
}

/// `cargo <args>` in the fixture crate, with `opt` broken when `broken`.
fn cargo(krate: &Path, target: &Path, args: &[&str], broken: bool) -> Output {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command
        .args(args)
        .current_dir(krate)
        .env("CARGO_TARGET_DIR", target)
        // simjig's own test models are for its repository; the fixture builds none.
        .env("SIMJIG_TEST_MODELS", "0")
        .env_remove("SIMJIG_CACHE_DIR");
    if broken {
        command.env("FIXTURE_BREAK", "1");
    } else {
        command.env_remove("FIXTURE_BREAK");
    }
    command.output().unwrap()
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
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fixture-optional");
    std::fs::create_dir_all(&root).unwrap();
    let lock = std::fs::File::create(root.join("lock")).unwrap();
    lock.lock().unwrap();
    let krate = prepare(&root);
    let target = root.join("target");
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
