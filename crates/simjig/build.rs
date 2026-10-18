//! Builds the C ABI test models that `tests/abi_counter.rs` links against.
//!
//! Cargo has no test-only build scripts, so this one runs for every build of `simjig`, but its
//! output reaches only integration-test binaries (`cargo::rustc-link-arg-tests`), never the
//! library or its users. When `simjig-build` grows the real Verilator driver, this moves there.
//!
//! The compile recipe (see `docs/notes/abi.md`) is the one simjig-build will use:
//! - every translation unit gets `-include simjig_vl_config.h` (the `VL_USER_*` hooks and the
//!   `VL_PRINTF` redirection), `-std=gnu++20` (coroutines for `--timing`) and `-Os`;
//! - libverilated (`verilated.cpp`, `verilated_threads.cpp`, `verilated_timing.cpp`) plus
//!   `simjig_rt.cpp` form one archive, shared by every model;
//! - each model's generated sources plus its shim form a second archive.
//!
//! If Verilator is missing, the script still succeeds (so the library builds anywhere) and
//! the test fails with the reason instead of being skipped.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The test models: SystemVerilog file stem, and whether it is verilated with `--timing`.
const MODELS: &[(&str, bool)] = &[("counter", false), ("ticker", true)];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(simjig_abi_models)");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=tests/abi_counter");
    println!("cargo::rerun-if-changed=../simjig-build/cpp");
    println!("cargo::rerun-if-env-changed=VERILATOR_ROOT");
    match build_models() {
        Ok(()) => println!("cargo::rustc-cfg=simjig_abi_models"),
        Err(err) => {
            let err = err.replace('\n', " ");
            println!("cargo::warning=simjig C ABI test models not built: {err}");
            println!("cargo::rustc-env=SIMJIG_ABI_MODELS_ERROR={err}");
        }
    }
}

fn build_models() -> Result<(), String> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let out = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let cpp = manifest.join("../simjig-build/cpp");
    let sv_dir = manifest.join("tests/abi_counter");
    let root = verilator_root()?;

    let mut model_dirs = Vec::new();
    for &(name, timing) in MODELS {
        model_dirs.push(verilate(
            &sv_dir.join(format!("{name}.sv")),
            name,
            timing,
            &out,
        )?);
    }

    let include = root.join("include");
    let mut runtime = base_build(&include, &cpp);
    runtime
        .file(include.join("verilated.cpp"))
        .file(include.join("verilated_threads.cpp"))
        .file(include.join("verilated_timing.cpp"))
        .file(cpp.join("simjig_rt.cpp"));
    runtime
        .try_compile("simjig_abi_verilated")
        .map_err(|e| e.to_string())?;

    let mut models = base_build(&include, &cpp);
    models.file(sv_dir.join("shim.cpp"));
    for dir in &model_dirs {
        models.include(dir);
        for file in generated_sources(dir)? {
            models.file(file);
        }
    }
    models
        .try_compile("simjig_abi_models")
        .map_err(|e| e.to_string())?;

    // Models first: on ELF targets an archive only satisfies references made before it.
    for lib in ["simjig_abi_models", "simjig_abi_verilated"] {
        println!(
            "cargo::rustc-link-arg-tests={}",
            out.join(format!("lib{lib}.a")).display()
        );
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo::rustc-link-arg-tests=-lc++");
    } else {
        println!("cargo::rustc-link-arg-tests=-lstdc++");
        println!("cargo::rustc-link-arg-tests=-lpthread");
    }
    Ok(())
}

/// `verilator --getenv VERILATOR_ROOT`, so the include directory matches the binary on PATH.
fn verilator_root() -> Result<PathBuf, String> {
    if let Some(root) = env::var_os("VERILATOR_ROOT") {
        return Ok(PathBuf::from(root));
    }
    let output = Command::new("verilator")
        .args(["--getenv", "VERILATOR_ROOT"])
        .output()
        .map_err(|e| format!("cannot run `verilator` ({e}); build inside `pixi run`"))?;
    if !output.status.success() {
        return Err("`verilator --getenv VERILATOR_ROOT` failed".to_owned());
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(PathBuf::from(root))
}

/// Runs `verilator --cc` for one model; returns its output directory.
fn verilate(sv: &Path, name: &str, timing: bool, out: &Path) -> Result<PathBuf, String> {
    let mdir = out.join(format!("V{name}"));
    let output = Command::new("verilator")
        .arg("--cc")
        .arg(if timing { "--timing" } else { "--no-timing" })
        .args(["--prefix", &format!("V{name}")])
        .arg("--Mdir")
        .arg(&mdir)
        .arg(sv)
        .output()
        .map_err(|e| format!("cannot run `verilator` ({e})"))?;
    if !output.status.success() {
        return Err(format!(
            "verilator failed on {}: {}",
            sv.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(mdir)
}

/// The `.cpp` files Verilator generated in `mdir`, sorted for a stable archive.
fn generated_sources(mdir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(mdir).map_err(|e| format!("{}: {e}", mdir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "cpp"))
        .collect();
    files.sort();
    Ok(files)
}

/// Flags shared by every translation unit (the compile recipe in the module docs).
fn base_build(include: &Path, cpp: &Path) -> cc::Build {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .cargo_metadata(false)
        .warnings(false)
        .opt_level_str("s")
        .flag("-std=gnu++20")
        .flag("-w")
        .flag("-include")
        .flag(cpp.join("simjig_vl_config.h").display().to_string())
        .include(include)
        .include(include.join("vltstd"))
        .include(cpp);
    // The VM_* switches verilated.mk passes everywhere. Of these, 5.052's runtime sources read
    // only VM_SC and VM_VPI, and generated code reads none, so one libverilated serves every
    // model; they are set for parity with Verilator's own build.
    for (key, value) in [
        ("VERILATOR", "1"),
        ("VM_COVERAGE", "0"),
        ("VM_SC", "0"),
        ("VM_TIMING", "1"),
        ("VM_TRACE", "0"),
        ("VM_TRACE_FST", "0"),
        ("VM_TRACE_VCD", "0"),
        ("VM_TRACE_SAIF", "0"),
        ("VM_VPI", "0"),
    ] {
        build.define(key, value);
    }
    build
}
