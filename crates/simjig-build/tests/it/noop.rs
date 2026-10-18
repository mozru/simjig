//! What a build script does not do again: a second DUT, or a rerun that changes nothing,
//! reuses what the first one found.

use simjig_build::Dut;

use crate::recipe::compiles;
use crate::support::{Sandbox, dut, logging_ccache};

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
