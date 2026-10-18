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
        msg.starts_with(
            "invalid DUT `probed` configuration: probe `lp.LP` (declared in build.rs): `LP` is \
             a localparam of `lp`"
        ) && msg.contains("probe it with `ProbeAccess::Read`"),
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
        msg.starts_with(
            "invalid DUT `probed` configuration: probe `lp.u_core.result` (declared in \
             build.rs): `result` is 8 bits wide in the design, but the probe's type is 5 bits"
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
        (
            "lp.u_core.result",
            "lp_cor",
            "no module `lp_cor` in the design; did you mean `lp_core`?",
        ),
        (
            "lp.u_core",
            "lp_core",
            "`lp.u_core` is an instance, not a variable; a probe path ends at a variable, \
             e.g. `lp.u_core.<variable>`",
        ),
    ] {
        // One sandbox each: a build script may build one DUT name in one configuration.
        let s = Sandbox::new();
        let msg = probed(&s, path, module, 8, ProbeAccess::Read);
        assert!(msg.contains(want), "{path}: {msg}");
        assert_eq!(s.verilations(), 0, "{path}: refused before verilating");
    }
}

fn gen_probe(s: &Sandbox, path: &str, module: &str, width: u32) -> Result<(), String> {
    Dut::new("gen")
        .sources([s.rtl().join("gen.sv")])
        .top("gen_pipe")
        .probe(
            path,
            module,
            ProbeType::Bits { width },
            ProbeAccess::ReadWrite,
        )
        .build_in(&s.env())
        .map(drop)
        .map_err(|e| format!("{e:?}"))
}

/// Instances inside a named generate block are reached through the block's name, and
/// Verilator's scope table agrees after verilating.
#[test]
fn a_probe_through_a_named_generate_block_builds() {
    let s = Sandbox::new();
    gen_probe(&s, "gen_pipe.g_x.u_x.acc", "gen_leaf", 8).unwrap();
}

/// An unnamed generate block is reached through the name Verilator elaborates for it
/// (`genblk<N>`, IEEE 1800-2023 §27.6).
#[test]
fn a_probe_through_an_unnamed_generate_block_builds() {
    let s = Sandbox::new();
    gen_probe(&s, "gen_pipe.genblk2.u_anon.s", "gen_leaf", 8).unwrap();
}

/// One iteration of a generate-for loop is named `g[1]`, and a variable declared in the
/// loop body belongs to the enclosing module.
#[test]
fn a_probe_through_a_generate_for_iteration_builds() {
    let s = Sandbox::new();
    gen_probe(&s, "gen_pipe.g[1].u.acc", "gen_leaf", 8).unwrap();
    let s = Sandbox::new();
    gen_probe(&s, "gen_pipe.g[0].t", "gen_pipe", 4).unwrap();
}

/// Generate mistakes name the fix before verilating: the loop's name without an index,
/// an index the loop does not have, and an instance array.
#[test]
fn generate_and_instance_array_mistakes_fail_the_build() {
    for (path, want) in [
        (
            "gen_pipe.g.u.acc",
            "`g` is a generate-for loop; name one iteration's scope as Verilator \
             elaborates it, e.g. `gen_pipe.g[0]`",
        ),
        (
            "gen_pipe.g[2].u.acc",
            "no instance or generate block `g[2]` in `gen_pipe` (a `gen_pipe`); did you \
             mean `g[0]`?",
        ),
        (
            "gen_pipe.arr.acc",
            "`arr` is an instance array; instance arrays are not supported in probe paths",
        ),
        (
            "gen_pipe.arr[0].acc",
            "`arr` is an instance array; instance arrays are not supported in probe paths",
        ),
        (
            "gen_pipe.g[0]",
            "`gen_pipe.g[0]` is a generate block, not a variable; a probe path ends at a \
             variable, e.g. `gen_pipe.g[0].<variable>`",
        ),
        (
            "gen_pipe.arr[1]",
            "`arr` is an instance array; instance arrays are not supported in probe paths",
        ),
    ] {
        let s = Sandbox::new();
        let msg = gen_probe(&s, path, "gen_leaf", 8).unwrap_err();
        assert!(msg.contains(want), "{path}: {msg}");
        assert_eq!(s.verilations(), 0, "{path}: refused before verilating");
    }
}

/// With `--output-split`, Verilator spreads the scope table over several `__Syms` files;
/// the check reads them all, so a good probe builds and a bad width is still caught.
#[test]
fn probes_are_checked_in_a_split_scope_table() {
    let build = |s: &Sandbox, width: u32| {
        Dut::new("probed")
            .sources([s.rtl().join("probed.sv")])
            .top("lp")
            .verilator_args(["--output-split", "1", "--output-split-cfuncs", "1"])
            .probe(
                "lp.u_core.result",
                "lp_core",
                ProbeType::Bits { width },
                ProbeAccess::ReadWrite,
            )
            .build_in(&s.env())
    };
    let s = Sandbox::new();
    let model = build(&s, 8).unwrap();
    let split = std::fs::read_dir(&model.obj_dir)
        .unwrap()
        .filter(|e| {
            let name = e.as_ref().unwrap().file_name();
            let name = name.to_string_lossy();
            name.contains("__Syms__ctor__") && name.ends_with(".cpp")
        })
        .count();
    assert!(split > 0, "the table is split");
    let s = Sandbox::new();
    let msg = format!("{:?}", build(&s, 5).unwrap_err());
    assert!(
        msg.contains("`result` is 8 bits wide in the design"),
        "{msg}"
    );
}

/// A `real` or `string` variable has no packed width (Verilator's scope table records it
/// with 0 packed dimensions, as if it were one bit): the build names its type instead of
/// suggesting a one-bit probe that `create` would then refuse.
#[test]
fn probes_of_a_real_or_a_string_fail_the_build() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    for (var, ty, width) in [("r", "real", 1), ("r", "real", 64), ("s", "string", 1)] {
        // One name per configuration in this build script.
        let name = format!("unpacked_{var}_{width}");
        let err = Dut::new(&name)
            .sources([rtl.join("unpacked_vars.sv")])
            .top("unpacked_vars")
            .probe(
                format!("unpacked_vars.{var}"),
                "unpacked_vars",
                ProbeType::Bits { width },
                ProbeAccess::Read,
            )
            .build_in(&s.env())
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.starts_with(&format!(
                "invalid DUT `{name}` configuration: probe `unpacked_vars.{var}` (declared \
                 in build.rs): `{var}` is a `{ty}`"
            )),
            "{msg}"
        );
    }
}

/// A build whose model check fails (here the probe check against the scope table) leaves
/// no IR in the cache: the entry is recorded only once the model agrees with it.
#[test]
fn a_failed_model_check_records_no_ir() {
    let s = Sandbox::new();
    let err = Dut::new("unpacked")
        .sources([s.rtl().join("unpacked_vars.sv")])
        .top("unpacked_vars")
        .probe(
            "unpacked_vars.r",
            "unpacked_vars",
            ProbeType::Bits { width: 1 },
            ProbeAccess::Read,
        )
        .build_in(&s.env())
        .unwrap_err();
    assert!(err.to_string().contains("is a `real`"), "{err}");
    let fronts = std::fs::read_dir(s.path().join("cache/fronts"))
        .unwrap()
        .count();
    assert_eq!(fronts, 0);
}
