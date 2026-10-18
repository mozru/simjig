//! Probe mistakes fail the build, before any model is created, with the probe's name and
//! the fix: path, instance and module mistakes in the lowering (against the IR), width and
//! access mistakes against Verilator's scope table after verilating.

use simjig_build::{Dut, ProbeAccess, ProbeType};

use crate::support::Sandbox;

fn probed(s: &Sandbox, path: &str, module: &str, width: u32, access: ProbeAccess) -> String {
    let rtl = s.rtl();
    let err = Dut::new("probed")
        .sources([rtl.join("probed.sv")])
        .top("lp")
        .probe(path, module, ProbeType::Bits { width }, access)
        .build_in(&s.env())
        .unwrap_err();
    format!("{err:?}")
}

/// A read-write probe of a localparam would hand the testbench a writable pointer to
/// constant storage (a deposit was SIGBUS); the build refuses it.
#[test]
fn read_write_probe_of_a_localparam_fails_the_build() {
    let s = Sandbox::new();
    let msg = probed(&s, "lp.LP", "lp", 8, ProbeAccess::ReadWrite);
    assert!(
        msg.contains("probe lp.LP: `LP` is a localparam of `lp`")
            && msg.contains("probe it with `ProbeAccess::Read`"),
        "{msg}"
    );
    assert_eq!(s.verilations(), 0, "refused before verilating");
}

/// The IR has no types for internal variables, so a wrong width is caught in Verilator's
/// scope table, right after verilating.
#[test]
fn probe_width_mismatch_fails_the_build() {
    let s = Sandbox::new();
    let msg = probed(&s, "lp.u_core.result", "lp_core", 5, ProbeAccess::Read);
    assert!(
        msg.contains(
            "probe `lp.u_core.result` (declared in build.rs): `result` is 8 bits wide in the \
             design, but the probe's type is 5 bits"
        ),
        "{msg}"
    );
}

/// Each path mistake names the fix, before verilating.
#[test]
fn probe_path_mistakes_fail_the_build() {
    for (path, module, want) in [
        (
            "lp.u_core.reslt",
            "lp_core",
            "no variable `reslt` in `lp_core` (the module of `lp.u_core`); did you mean `result`?",
        ),
        (
            "lp.u_cre.result",
            "lp_core",
            "no instance `u_cre` in `lp` (a `lp`); did you mean `u_core`?",
        ),
        (
            "lp.u_core.result",
            "lp",
            "`lp.u_core` is an instance of `lp_core`, not of `lp` as the probe declares",
        ),
    ] {
        // One sandbox each: a build script may build one DUT name in one configuration.
        let s = Sandbox::new();
        let msg = probed(&s, path, module, 8, ProbeAccess::Read);
        assert!(msg.contains(want), "{path}: {msg}");
        assert_eq!(s.verilations(), 0, "{path}: refused before verilating");
    }
}
