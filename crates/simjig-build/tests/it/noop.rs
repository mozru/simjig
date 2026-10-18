//! What a build script does not do again: a second DUT, or a rerun that changes nothing,
//! reuses what the first one found.

use simjig_build::Dut;

use crate::recipe::compiles;
use crate::support::{Sandbox, dut, logging_ccache, write_script};

fn build(name: &str, env: &simjig_build::BuildEnv) -> simjig_build::Built {
    Dut::new(name)
        .sources([dut(name).join(format!("{name}.sv"))])
        .top(name)
        .build_in(env)
        .unwrap()
}

/// The second DUT of a build script links the Verilator runtime the first one found,
/// without looking it up again: here the cache has lost its manifests in between, and
/// still no runtime source is compiled again.
#[test]
fn the_runtime_is_looked_up_once_per_build_script() {
    let s = Sandbox::new();
    let (ccache, log) = logging_ccache(&s);
    let mut env = s.env();
    env.simjig_ccache = Some(ccache.into());
    build("inv", &env);
    std::fs::remove_dir_all(s.path().join("cache/manifests")).unwrap();
    std::fs::remove_dir_all(&log).unwrap();
    std::fs::create_dir_all(&log).unwrap();
    let flop = build("flop", &env);
    let names: Vec<String> = compiles(&log).into_iter().map(|(n, _)| n).collect();
    assert!(
        names.iter().all(|n| n.starts_with(&flop.prefix)),
        "only the new model is compiled: {names:?}"
    );
}

/// A build script asks Verilator for its installation once, not once per DUT.
#[test]
fn verilator_is_located_once_per_build_script() {
    let s = Sandbox::new();
    let env = s.env();
    build("inv", &env);
    build("flop", &env);
    let asks = s
        .verilator_calls()
        .lines()
        .filter(|l| l.starts_with("--getenv"))
        .count();
    assert_eq!(asks, 1, "{}", s.verilator_calls());
}

/// The C++ compiler is set up (found, its kind detected, its version asked) once per
/// build script: building a DUT again, with every archive in the cache, runs it no more.
#[test]
fn the_compiler_is_set_up_once_per_build_script() {
    let s = Sandbox::new();
    let name = std::env::var("CXX").unwrap_or_else(|_| "c++".to_owned());
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from(&name));
    let runs = s.path().join("cxx-runs");
    std::fs::create_dir_all(&runs).unwrap();
    let cxx = s.path().join("logging-cxx");
    write_script(
        &cxx,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'/$$\nexec '{}' \"$@\"\n",
            runs.display(),
            real.display()
        ),
    );
    // SAFETY: nextest runs every test in its own process, and nothing else runs yet.
    unsafe { std::env::set_var("CXX", &cxx) };
    let mut env = s.env();
    env.cxx = Some(cxx.into_os_string());
    env.simjig_ccache = Some("0".into());
    build("inv", &env);
    std::fs::remove_dir_all(&runs).unwrap();
    std::fs::create_dir_all(&runs).unwrap();
    build("inv", &env);
    let ran: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(ran, Vec::<String>::new(), "the compiler ran again");
}
