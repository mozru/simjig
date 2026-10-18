//! Coverage counters at run time, with the pinned Verilator.

use std::fs;

use simjig_build::codegen::{ShimOptions, lower};

use crate::cases;
use crate::probes::{build_driver, opts};

/// The sum of the counts in a `coverage.dat` (`C '<keys>' <count>` per point).
fn dat_total(text: &str) -> u64 {
    text.lines()
        .filter(|l| l.starts_with("C '"))
        .map(|l| l.rsplit(' ').next().unwrap().parse::<u64>().unwrap())
        .sum()
}

/// An interface-ported DUT keeps each interface instance as its own module object, so its
/// counters are split across objects; `cov_runs` returns every one of them.
#[test]
fn cov_runs_cover_the_counters_of_an_interface_ported_dut() {
    let case = cases::iface_top();
    let lowered = lower(&case.design, &case.config).unwrap();
    let shim_opts = ShimOptions {
        coverage: true,
        ..opts("Viface_cov")
    };
    let driver = build_driver(&case, &lowered, &shim_opts, &["--coverage"]);
    let dat = driver.exe.with_file_name("cov.dat");
    let cov = format!("cov:{}:0", dat.display());
    let w = |sig: &str, v: u32| format!("w:simjig_wrap_iface_top.{sig}:{v:x}");
    let mut ops = vec![
        w("rst", 1),
        w("clk", 0),
        "e".into(),
        w("clk", 1),
        "e".into(),
    ];
    ops.extend([w("rst", 0), w("in_bus_valid", 1), w("out_bus_ready", 1)]);
    for i in 0..6 {
        ops.extend([
            w("in_bus_data", i),
            w("clk", 0),
            "e".into(),
            w("clk", 1),
            "e".into(),
        ]);
    }
    ops.push(cov);
    let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
    let (ok, stdout) = driver.run(&ops);
    assert!(ok, "{stdout}");
    let line = stdout.lines().find(|l| l.starts_with("cov ")).unwrap();
    let nums: Vec<u64> = line.split(' ').filter_map(|w| w.parse().ok()).collect();
    let [runs, counters, sum] = nums[..] else {
        panic!("{line}")
    };
    // The top object and the two bus_if instances at least.
    assert!(runs >= 3, "{line}");
    assert!(counters > 0 && sum > 0, "{line}");
    // Each counter is at least one point of coverage.dat (aliases share one).
    let total = dat_total(&fs::read_to_string(&dat).unwrap());
    assert!(sum <= total, "{line}, coverage.dat total {total}");
}
