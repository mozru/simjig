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
