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
/// A build script that handles `Dut::build`'s error (an optional model) and exits 0
/// builds: `Dut::build` printed the error as `cargo::error=`, which fails the build even
/// then. Its cause is warned once, by `Dut::build`, not again by the script.
#[test]
fn a_handled_build_error_does_not_fail_the_build() {
    let (krate, target, _lock) = fixture();
    let envs = [
        (
            "SIMJIG_CACHE_DIR",
            krate.parent().unwrap().join("cache").into_os_string(),
        ),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    // `cargo_with` checks that the build succeeded.
    let (_, stderr) = cargo_with(&krate, &target, &["build", "--offline", "-vv"], &envs);
    // The cause is warned once, by `Dut::build`, and the script's own warning only says
    // what it skipped (`-vv` also echoes the raw `cargo::` lines, which are not counted).
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("warning: "))
        .collect();
    let cause = warnings
        .iter()
        .filter(|l| l.contains("invalid DUT `optional` configuration"))
        .count();
    assert_eq!(cause, 1, "{stderr}");
    assert!(
        warnings
            .iter()
            .any(|l| l.ends_with(": optional DUT skipped")),
        "{stderr}"
    );
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

/// s3c6r: a script that handles a failed `Dut::build` (an optional model) and exits 0
/// must still rerun when that DUT's source is fixed. `Dut::build` prints nothing on
/// `Err`, so only the other DUTs' `rerun-if-changed` lines reach Cargo.
#[test]
fn s3c6r_fixing_a_handled_failed_duts_source_reruns_the_script() {
    let (krate, target, _lock) = fixture();
    let envs = [
        (
            "SIMJIG_CACHE_DIR",
            krate.parent().unwrap().join("cache").into_os_string(),
        ),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    let opt = krate.join("duts/opt");
    std::fs::create_dir_all(&opt).unwrap();
    let source = opt.join("opt.sv");
    std::fs::write(
        &source,
        "module opt(input logic a, output logic y);\n  assign y = ;\nendmodule\n",
    )
    .unwrap();
    let script = krate.join("build.rs");
    let text = std::fs::read_to_string(&script).unwrap().replace(
        r#"Dut::new("optional").top("optional").build()"#,
        r#"Dut::new("opt").sources(["duts/opt/opt.sv"]).top("opt").build()"#,
    );
    std::fs::write(&script, text).unwrap();
    assert!(build(&krate, &target, &envs), "the edited script runs");
    assert!(!build(&krate, &target, &envs), "no change: no rerun");
    std::fs::write(
        &source,
        "module opt(input logic a, output logic y);\n  assign y = ~a;\nendmodule\n",
    )
    .unwrap();
    assert!(
        build(&krate, &target, &envs),
        "fixing the failed DUT's source must rerun the build script"
    );
}

/// s3c7m: a handled failed DUT whose sources come from a `-f` file list (the usual
/// Verilator flow) or a `-y` library directory must rerun when a listed source is fixed.
fn s3c7m_failed_dut_reruns_via(args: &str, extra: &dyn Fn(&Path)) {
    let (krate, target, _lock) = fixture();
    let envs = [
        (
            "SIMJIG_CACHE_DIR",
            krate.parent().unwrap().join("cache").into_os_string(),
        ),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    let opt = krate.join("duts/opt");
    std::fs::create_dir_all(&opt).unwrap();
    let source = opt.join("opt.sv");
    std::fs::write(
        &source,
        "module opt(input logic a, output logic y);\n  assign y = ;\nendmodule\n",
    )
    .unwrap();
    extra(&opt);
    let script = krate.join("build.rs");
    let text = std::fs::read_to_string(&script).unwrap().replace(
        r#"Dut::new("optional").top("optional").build()"#,
        &format!(r#"Dut::new("opt"){args}.top("opt").build()"#),
    );
    std::fs::write(&script, text).unwrap();
    assert!(build(&krate, &target, &envs), "the edited script runs");
    assert!(!build(&krate, &target, &envs), "no change: no rerun");
    std::fs::write(
        &source,
        "module opt(input logic a, output logic y);\n  assign y = ~a;\nendmodule\n",
    )
    .unwrap();
    assert!(
        build(&krate, &target, &envs),
        "fixing the failed DUT's source ({args}) must rerun the build script"
    );
}

#[test]
fn s3c7m_a_failed_dut_from_a_file_list_reruns_when_fixed() {
    s3c7m_failed_dut_reruns_via(r#".verilator_args(["-f", "duts/opt/files.f"])"#, &|opt| {
        std::fs::write(opt.join("files.f"), "duts/opt/opt.sv\n").unwrap();
    });
}

#[test]
fn s3c7m_a_failed_dut_from_a_library_dir_reruns_when_fixed() {
    s3c7m_failed_dut_reruns_via(
        r#".sources(["duts/opt/top_stub.sv"]).verilator_args(["-y", "duts/opt"])"#,
        &|opt| {
            std::fs::write(opt.join("top_stub.sv"), "module stub; endmodule\n").unwrap();
        },
    );
}

/// A handled failed DUT whose arguments name the package directory (`+incdir+.`,
/// common in file lists) with Cargo's default `target/` inside the package: the tracked
/// directory holds the build's own outputs, so every build reruns the script and
/// recompiles the package, though nothing the DUT reads changed.
#[test]
fn a_failed_dut_tracking_the_package_dir_settles() {
    let (krate, _, _lock) = fixture();
    // The default layout: the target directory inside the package.
    let target = krate.join("target");
    let envs = [
        (
            "SIMJIG_CACHE_DIR",
            krate.parent().unwrap().join("cache").into_os_string(),
        ),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    let opt = krate.join("duts/opt");
    std::fs::create_dir_all(&opt).unwrap();
    std::fs::write(
        opt.join("opt.sv"),
        "module opt(input logic a, output logic y);\n  assign y = ;\nendmodule\n",
    )
    .unwrap();
    let script = krate.join("build.rs");
    let text = std::fs::read_to_string(&script).unwrap().replace(
        r#"Dut::new("optional").top("optional").build()"#,
        r#"Dut::new("opt").sources(["duts/opt/opt.sv"]).verilator_args(["+incdir+."]).top("opt").build()"#,
    );
    std::fs::write(&script, text).unwrap();
    assert!(build(&krate, &target, &envs), "the edited script runs");
    assert!(!build(&krate, &target, &envs), "no change: no rerun");
}

/// s3c9 repro: a *successful* DUT whose C++ recipe names the package directory as an
/// include directory (`include_dirs: [\".\"]`, a header next to `build.rs`) with Cargo's
/// default `target/` inside the package: the success path tracks the directory whole, so
/// every build reruns the script.
#[test]
fn s3c9_a_built_dut_with_the_package_as_a_cpp_include_dir_settles() {
    let (krate, _, _lock) = fixture();
    let target = krate.join("target");
    let envs = [
        (
            "SIMJIG_CACHE_DIR",
            krate.parent().unwrap().join("cache").into_os_string(),
        ),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    let script = krate.join("build.rs");
    let text = std::fs::read_to_string(&script).unwrap().replace(
        r#"dut("inv").build()?;"#,
        r#"dut("inv").recipe(simjig_build::CppRecipe { include_dirs: vec![".".into()], ..Default::default() }).build()?;"#,
    );
    assert!(text.contains("CppRecipe"));
    std::fs::write(&script, text).unwrap();
    assert!(build(&krate, &target, &envs), "the edited script runs");
    assert!(!build(&krate, &target, &envs), "no change: no rerun");
}

/// The newest build-script output (`target/debug/build/<pkg>-*/output`): what the last run
/// printed to Cargo.
fn last_output(target: &Path) -> String {
    let newest = out_dirs(target)
        .into_iter()
        .map(|d| d.with_file_name("output"))
        .filter(|f| f.is_file())
        .max_by_key(|f| f.metadata().unwrap().modified().unwrap())
        .unwrap();
    std::fs::read_to_string(newest).unwrap()
}

/// A model reruns its script on the user's C++ headers, not on the toolchain's (the C++
/// library, the SDK, the compiler's prefix), which the compiler's identity covers: those
/// were most of a failed build's `--- stdout` dump. A rerun with no change stays a
/// no-op, and an edit to the user's header still reruns.
#[test]
fn a_model_reruns_on_the_users_headers_and_not_the_toolchains() {
    let (krate, _, _lock) = fixture();
    let target = krate.parent().unwrap().join("target");
    let envs = [
        (
            "SIMJIG_CACHE_DIR",
            krate.parent().unwrap().join("cache").into_os_string(),
        ),
        ("SIMJIG_CCACHE", "0".into()),
    ];
    let cpp = krate.join("cpp");
    std::fs::create_dir_all(&cpp).unwrap();
    let header = cpp.join("hook.h");
    std::fs::write(
        &header,
        "#include <vector>\ninline int hook() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        cpp.join("hook.cpp"),
        "#include \"hook.h\"\nint simjig_fixture_hook() { return hook(); }\n",
    )
    .unwrap();
    let script = krate.join("build.rs");
    let text = std::fs::read_to_string(&script).unwrap().replace(
        r#"dut("inv").build()?;"#,
        r#"dut("inv").recipe(simjig_build::CppRecipe { model_sources: vec!["cpp/hook.cpp".into()], include_dirs: vec!["cpp".into()], ..Default::default() }).build()?;"#,
    );
    assert!(text.contains("hook.cpp"));
    std::fs::write(&script, text).unwrap();
    assert!(build(&krate, &target, &envs), "the edited script runs");
    let reruns: Vec<String> = last_output(&target)
        .lines()
        .filter_map(|l| l.strip_prefix("cargo::rerun-if-changed="))
        .map(str::to_owned)
        .collect();
    assert!(
        reruns.iter().any(|r| Path::new(r).ends_with("cpp/hook.h")),
        "{reruns:#?}"
    );
    // libc++ and libstdc++ both live in `<dir>/include/c++/`; `<vector>` reached them.
    let toolchain: Vec<&String> = reruns
        .iter()
        .filter(|r| r.contains("/include/c++/") || r.contains(".sdk/"))
        .collect();
    assert!(toolchain.is_empty(), "{toolchain:#?}");
    assert!(!build(&krate, &target, &envs), "no change: no rerun");
    std::fs::write(
        &header,
        "#include <vector>\ninline int hook() { return 2; }\n",
    )
    .unwrap();
    assert!(
        build(&krate, &target, &envs),
        "an edit to the user's header reruns the script"
    );
}
