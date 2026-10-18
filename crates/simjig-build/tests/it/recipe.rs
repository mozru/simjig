//! The C ABI compile recipe (docs/notes/abi.md), observed on the real compiler command
//! lines through a logging ccache stand-in.

use simjig_build::Dut;

use crate::support::{Sandbox, dut, logging_ccache};

/// Every compile command of one build, as (source file, command line).
fn compiles(log: &std::path::Path) -> Vec<(String, String)> {
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
