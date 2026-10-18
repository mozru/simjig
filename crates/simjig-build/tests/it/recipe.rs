//! The C ABI compile recipe (docs/notes/abi.md), observed on the real compiler command
//! lines through a logging ccache stand-in.

use simjig_build::{CppRecipe, Dut};

use crate::support::{Sandbox, dut, logging_ccache};

/// Every compile command of one build, as (source file, command line).
pub(crate) fn compiles(log: &std::path::Path) -> Vec<(String, String)> {
    let lines: Vec<String> = std::fs::read_dir(log)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    lines
        .iter()
        .flat_map(|l| l.lines())
        .filter(|l| l.split(' ').any(|a| a == "-c"))
        .map(|l| {
            let src = l
                .split(' ')
                .find(|a| {
                    std::path::Path::new(a)
                        .extension()
                        .is_some_and(|e| e == "cpp")
                })
                .unwrap_or("?");
            let name = std::path::Path::new(src).file_name().unwrap();
            (name.to_string_lossy().into_owned(), l.to_owned())
        })
        .collect()
}

/// The first `-O` flag: the one simjig-build sets (CXXFLAGS from the environment come
/// later and win, per the `cc` crate's order).
fn first_opt(line: &str) -> &str {
    line.split(' ')
        .find(|a| a.starts_with("-O"))
        .unwrap_or_else(|| panic!("no -O flag: {line}"))
}

#[test]
fn every_translation_unit_gets_the_abi_flags() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    let mut env = s.env();
    env.simjig_ccache = Some(ccache.into());
    let built = Dut::new("counter")
        .sources([dut("counter").join("counter.sv")])
        .top("counter")
        .build_in(&env)
        .unwrap();
    let units = compiles(&log);
    let names: Vec<&str> = units.iter().map(|(n, _)| n.as_str()).collect();
    for want in [
        "verilated.cpp",
        "verilated_threads.cpp",
        "verilated_timing.cpp",
        "simjig_rt.cpp",
    ] {
        assert!(names.contains(&want), "{want} not compiled: {names:?}");
    }
    assert!(
        names.iter().any(|n| n.starts_with(&built.prefix)),
        "{names:?}"
    );
    for (name, line) in &units {
        assert!(
            line.contains(" -include ") && line.contains("/simjig_vl_config.h "),
            "{name}: {line}"
        );
        assert!(line.contains(" -std=gnu++20 "), "{name}: {line}");
        assert_eq!(first_opt(line), "-O2", "{name}: -O2 regardless of profile");
        assert!(line.contains(" -DVERILATOR=1 "), "{name}: {line}");
        // The shared runtime has fixed switches; the model has its own (no --timing).
        let timing = if name.starts_with("verilated") || name == "simjig_rt.cpp" {
            " -DVM_TIMING=1 "
        } else {
            " -DVM_TIMING=0 "
        };
        assert!(line.contains(timing), "{name}: {line}");
    }
}

#[test]
fn simjig_cxx_opt_overrides_the_level() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    let mut env = s.env();
    env.simjig_ccache = Some(ccache.into());
    env.cxx_opt = "1".into();
    Dut::new("inv")
        .sources([dut("inv").join("inv.sv")])
        .top("inv")
        .build_in(&env)
        .unwrap();
    let units = compiles(&log);
    assert_ne!(units.len(), 0);
    for (name, line) in &units {
        assert_eq!(first_opt(line), "-O1", "{name}: {line}");
    }
}

/// Verilator's own fatal paths (for example a misused `nextTimeSlot()`) call `vl_fatal`
/// from inside libverilated, so the hooks must live in the shared archive, and only in
/// `simjig_rt.o`: `VL_USER_*` must have removed the defaults from `verilated.o`.
#[test]
fn the_fatal_hooks_are_defined_once_in_the_shared_archive() {
    let s = Sandbox::new();
    let built = Dut::new("inv")
        .sources([dut("inv").join("inv.sv")])
        .top("inv")
        .build_in(&s.env())
        .unwrap();
    let archive = built
        .directives
        .iter()
        .filter_map(|d| d.strip_prefix("cargo::rustc-link-search=native="))
        .map(std::path::PathBuf::from)
        .find(|d| d.starts_with(s.path().join("cache/libverilated")))
        .and_then(|d| {
            std::fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|e| e == "a"))
        })
        .unwrap();
    let nm = std::process::Command::new("nm")
        .arg("-A")
        .arg(&archive)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&nm.stdout);
    for hook in ["vl_fatal", "vl_finish", "vl_stop_maybe", "vl_warn"] {
        let defs: Vec<&str> = text
            .lines()
            .filter(|l| l.contains(" T ") && l.contains(hook) && l.contains("PKci"))
            .collect();
        assert_eq!(defs.len(), 1, "{hook}: {defs:?}");
        assert!(defs[0].contains("simjig_rt.o"), "{hook}: {defs:?}");
    }
}

/// A build script reruns whenever any of its DUTs changes, and builds every DUT again. A
/// model's compiled archive is cached under its key, so only the edited DUT's C++ is
/// compiled again: the other model, whose Verilator output, shim and toolchain are the
/// same, is reused from the cache.
#[test]
fn editing_one_dut_recompiles_only_its_model() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    let mut env = s.env();
    env.simjig_ccache = Some(ccache.into());
    let rtl = s.path().join("duts");
    for d in ["inv", "flop"] {
        crate::support::copy_dir(&dut(d), &rtl.join(d));
    }
    let build = |name: &str| {
        Dut::new(name)
            .sources([rtl.join(name).join(format!("{name}.sv"))])
            .top(name)
            .build_in(&env)
            .unwrap()
    };
    build("inv");
    let flop = build("flop");
    std::fs::remove_dir_all(&log).unwrap();
    std::fs::create_dir_all(&log).unwrap();
    // The build script runs again after an edit to `inv`: both DUTs are built.
    let inv_sv = rtl.join("inv/inv.sv");
    let text = std::fs::read_to_string(&inv_sv).unwrap();
    std::fs::write(&inv_sv, format!("{text}\n// edited\n")).unwrap();
    let inv = build("inv");
    assert_eq!(build("flop").prefix, flop.prefix);
    let names: Vec<String> = compiles(&log).into_iter().map(|(n, _)| n).collect();
    assert!(
        names.iter().any(|n| n.starts_with(&inv.prefix)),
        "the edited model is compiled: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with(&flop.prefix)),
        "the unchanged model is not: {names:?}"
    );
}

/// A recipe model source in `<sandbox>/src` that includes `header` (`K` is its value).
fn recipe_source(s: &Sandbox, file: &str, header: &str) -> std::path::PathBuf {
    let src = s.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let path = src.join(file);
    std::fs::write(
        &path,
        format!(
            "#include \"{header}\"\nint simjig_test_{}() {{ return K; }}\n",
            &file[..file.len() - 4]
        ),
    )
    .unwrap();
    path
}

/// Builds `inv` with `recipe`; returns the names of the files compiled.
fn build_inv(
    s: &Sandbox,
    env: &simjig_build::BuildEnv,
    log: &std::path::Path,
    recipe: CppRecipe,
) -> (Vec<String>, Vec<String>) {
    std::fs::remove_dir_all(log).unwrap();
    std::fs::create_dir_all(log).unwrap();
    let built = Dut::new("inv")
        .sources([dut("inv").join("inv.sv")])
        .top("inv")
        .recipe(recipe)
        .build_in(env)
        .unwrap();
    let _ = s;
    (
        compiles(log).into_iter().map(|(n, _)| n).collect(),
        built.directives,
    )
}

/// A header in a recipe include directory is part of the model's archive key: editing it
/// compiles the model again.
#[test]
fn editing_a_header_in_a_recipe_include_dir_recompiles_the_model() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    let mut env = s.env();
    env.simjig_ccache = Some(ccache.into());
    let inc = s.path().join("inc");
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::write(inc.join("k.h"), "#define K 1\n").unwrap();
    let recipe = CppRecipe {
        model_sources: vec![recipe_source(&s, "uses_k.cpp", "k.h")],
        include_dirs: vec![inc.clone()],
        ..CppRecipe::default()
    };
    let (first, _) = build_inv(&s, &env, &log, recipe.clone());
    assert!(first.contains(&"uses_k.cpp".to_owned()), "{first:?}");
    let (again, _) = build_inv(&s, &env, &log, recipe.clone());
    assert!(again.is_empty(), "nothing changed: {again:?}");
    std::fs::write(inc.join("k.h"), "#define K 2\n").unwrap();
    let (edited, _) = build_inv(&s, &env, &log, recipe);
    assert!(edited.contains(&"uses_k.cpp".to_owned()), "{edited:?}");
}

/// A header found next to a recipe source (not in any include directory) is a dependency
/// too: editing it compiles the model again, and the build script reruns on it.
#[test]
fn editing_a_header_next_to_a_recipe_source_recompiles_the_model() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    let mut env = s.env();
    env.simjig_ccache = Some(ccache.into());
    let source = recipe_source(&s, "uses_near.cpp", "near.h");
    let near = source.with_file_name("near.h");
    std::fs::write(&near, "#define K 1\n").unwrap();
    let recipe = CppRecipe {
        model_sources: vec![source],
        ..CppRecipe::default()
    };
    let (first, directives) = build_inv(&s, &env, &log, recipe.clone());
    assert!(first.contains(&"uses_near.cpp".to_owned()), "{first:?}");
    let rerun = format!("cargo::rerun-if-changed={}", near.display());
    assert!(directives.contains(&rerun), "{directives:?}");
    let (again, directives) = build_inv(&s, &env, &log, recipe.clone());
    assert!(again.is_empty(), "nothing changed: {again:?}");
    assert!(
        directives.contains(&rerun),
        "a cache hit reruns on it too: {directives:?}"
    );
    std::fs::write(&near, "#define K 2\n").unwrap();
    let (edited, _) = build_inv(&s, &env, &log, recipe);
    assert!(edited.contains(&"uses_near.cpp".to_owned()), "{edited:?}");
}

/// The compiler's identity is part of every archive key: a compiler whose `--version`
/// changes (an upgrade in place, same path) compiles the model and the runtime again.
#[test]
fn a_new_compiler_version_recompiles_every_archive() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    // The compiler `cc` would pick: `CXX` (pixi sets it), else `c++`, found on PATH.
    let name = std::env::var("CXX").unwrap_or_else(|_| "c++".to_owned());
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from(&name));
    let version = s.path().join("cxx-version");
    std::fs::write(&version, "fake c++ 1.0\n").unwrap();
    let cxx = s.path().join("fake-cxx");
    crate::support::write_script(
        &cxx,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then cat '{}'; exit 0; fi\nexec '{}' \"$@\"\n",
            version.display(),
            real.display()
        ),
    );
    // SAFETY: nextest runs every test in its own process, and nothing else runs yet.
    unsafe { std::env::set_var("CXX", &cxx) };
    // Each build stands for one build-script run (one OUT_DIR), which asks the compiler
    // for its version once.
    let env_in = |out: &str| {
        let mut env = s.env();
        env.out_dir = s.path().join(out);
        env.simjig_ccache = Some(ccache.clone().into());
        env
    };
    let (first, _) = build_inv(&s, &env_in("out1"), &log, CppRecipe::default());
    assert!(first.contains(&"verilated.cpp".to_owned()), "{first:?}");
    let (same, _) = build_inv(&s, &env_in("out2"), &log, CppRecipe::default());
    assert!(
        !same.contains(&"verilated.cpp".to_owned()),
        "the same compiler: {same:?}"
    );
    std::fs::write(&version, "fake c++ 1.1\n").unwrap();
    let (upgraded, _) = build_inv(&s, &env_in("out3"), &log, CppRecipe::default());
    assert!(
        upgraded.contains(&"verilated.cpp".to_owned())
            && upgraded.iter().any(|n| n.starts_with('V')),
        "{upgraded:?}"
    );
}

/// Recipe flags belong to the DUT: they reach the model's sources and not the shared
/// Verilator runtime, so two DUTs with different flags still link one libverilated (two
/// archives of it in one binary would define the same symbols).
#[test]
fn recipe_flags_reach_the_model_only() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    let mut env = s.env();
    env.simjig_ccache = Some(ccache.into());
    let mut prefixes = Vec::new();
    for (name, value) in [("inv", 1), ("flop", 2)] {
        let built = Dut::new(name)
            .sources([dut(name).join(format!("{name}.sv"))])
            .top(name)
            .recipe(CppRecipe {
                flags: vec![format!("-DSIMJIG_RECIPE_FLAG={value}")],
                ..CppRecipe::default()
            })
            .build_in(&env)
            .unwrap();
        prefixes.push(built.prefix);
    }
    for (name, line) in compiles(&log) {
        let model = prefixes.iter().any(|p| name.starts_with(p.as_str()));
        assert_eq!(
            line.contains("-DSIMJIG_RECIPE_FLAG="),
            model,
            "{name}: {line}"
        );
    }
    let runtimes = std::fs::read_dir(s.path().join("cache/libverilated"))
        .unwrap()
        .count();
    assert_eq!(runtimes, 1, "one shared libverilated");
}
