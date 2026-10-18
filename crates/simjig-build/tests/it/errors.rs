//! Failures carry the command line, the stderr excerpt and the offending path.

use simjig_build::{Dut, Error, Verilator};

use crate::support::{Sandbox, write_script};

#[test]
fn verilator_errors_name_the_command_and_file() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let broken = rtl.join("broken.sv");
    let err = Dut::new("broken")
        .sources([&broken])
        .top("broken")
        .build_in(&s.env())
        .unwrap_err();
    let Error::Command {
        command,
        stderr,
        path,
        ..
    } = &err
    else {
        panic!("expected a command error, got {err}");
    };
    // The JSON pass runs first, so it is the one that reports the syntax error.
    assert!(
        command.contains(" --json-only ") && command.contains("broken.sv"),
        "{command}"
    );
    assert!(stderr.contains("%Error"), "{stderr}");
    assert_eq!(path.as_deref(), Some(broken.as_path()));
    let msg = err.to_string();
    assert!(
        msg.starts_with(&format!(
            "parsing DUT `broken` (Verilator JSON pass) failed (exit status: 1) on `{}`",
            broken.display()
        )),
        "{msg}"
    );
    assert!(msg.ends_with(&format!("\ncommand: {command}")), "{msg}");

    let models = s.path().join("cache/models");
    assert_eq!(
        std::fs::read_dir(models).unwrap().count(),
        0,
        "no entry for a failed run"
    );
    assert_eq!(
        std::fs::read_dir(s.path().join("cache/tmp"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn unsupported_verilator_versions_are_rejected() {
    let s = Sandbox::new();
    let fake = s.path().join("old-verilator");
    write_script(
        &fake,
        "#!/bin/sh\necho 'Verilator 5.050 2025-01-01 rev v5.050'\n",
    );
    let mut env = s.env();
    env.simjig_verilator = Some(fake.clone());
    let err = Dut::new("c")
        .sources(["x.sv"])
        .top("c")
        .build_in(&env)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "unsupported Verilator at {}: `Verilator 5.050 2025-01-01 rev v5.050`; \
             supported versions: 5.052; install Verilator 5.052 (in the simjig repo: \
             `pixi install --locked`)",
            fake.display()
        )
    );
}

/// `SIMJIG_UNTESTED_VERILATOR=1` accepts an unsupported release, as `cargo simjig doctor`
/// does (one rule: `svtypes::toolchain::judge`), and the build then warns.
#[test]
fn the_untested_override_accepts_an_unsupported_release() {
    let s = Sandbox::new();
    let fake = s.path().join("old-verilator");
    write_script(
        &fake,
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'Verilator 5.050 2025-01-01 rev v5.050';;\n  \
         --getenv) echo /opt/old;;\nesac\n",
    );
    let mut env = s.env();
    env.simjig_verilator = Some(fake.clone());
    assert!(matches!(
        Verilator::locate(&env),
        Err(Error::UnsupportedVerilator { .. })
    ));
    env.untested_verilator = true;
    let v = Verilator::locate(&env).unwrap();
    assert!(v.untested, "{v:?}");
    assert_eq!(v.version.to_string(), "5.050");
    assert_eq!(v.include_dir, std::path::Path::new("/opt/old/include"));
}

#[test]
fn an_explicit_verilator_that_does_not_exist_is_an_error() {
    let s = Sandbox::new();
    let mut env = s.env();
    let missing = s.path().join("nope/verilator");
    env.simjig_verilator = Some(missing.clone());
    let err = Dut::new("c")
        .sources(["x.sv"])
        .top("c")
        .build_in(&env)
        .unwrap_err();
    assert!(
        matches!(&err, Error::VerilatorNotFound { searched } if searched == &[missing]),
        "{err}"
    );

    // Without overrides, PATH is searched and every candidate is reported.
    env.simjig_verilator = None;
    env.path = Some(s.path().join("empty-bin").into_os_string());
    let err = Dut::new("c")
        .sources(["x.sv"])
        .top("c")
        .build_in(&env)
        .unwrap_err();
    assert!(err.to_string().contains("empty-bin/verilator"), "{err}");
}

/// A design mixing units with and without a `timeunit` fails with a hint naming the unit
/// that has none and the `--timescale` default (Verilator 5.052's `TIMESCALEMOD`).
#[test]
fn a_mixed_timeunit_design_names_the_unit_to_fix() {
    let s = Sandbox::new();
    let leaf = s.path().join("leaf.sv");
    let top = s.path().join("top.sv");
    std::fs::write(
        &leaf,
        "module leaf(input logic a, output logic y);\n  assign y = a;\nendmodule\n",
    )
    .unwrap();
    std::fs::write(
        &top,
        "module top(input logic a, output logic y);\n  timeunit 1ns; timeprecision 1ps;\n  \
         leaf u(.a, .y);\nendmodule\n",
    )
    .unwrap();
    let err = Dut::new("mixed")
        .sources([&leaf, &top])
        .top("top")
        .build_in(&s.env())
        .unwrap_err()
        .to_string();
    let hint = format!(
        "hint: some units declare a `timeunit` and these do not: `{}:1`; add a `timeunit` to \
         each, or give every unit a default with `.verilator_args([\"--timescale\", \
         \"1ns/1ps\"])` on the `Dut`",
        leaf.display()
    );
    assert!(err.contains(&hint), "{err}");
}

/// A DUT with interface ports and many data ports, built with `-Wall`, whose design has a
/// real error: the `PINMISSING` warnings of the generated elaboration top (one per open
/// data port) are dropped, the error leads the message, on its first line too (what a
/// script handling the failure prints), and no lint waiver is suggested.
#[test]
fn a_real_error_under_wall_is_not_buried_by_the_generated_top() {
    let s = Sandbox::new();
    let iface = s.path().join("w_if.sv");
    let top = s.path().join("w_top.sv");
    std::fs::write(
        &iface,
        "interface w_if;\n  logic v;\n  modport sink (input v);\nendinterface\n",
    )
    .unwrap();
    let outs: String = (0..12).map(|i| format!(", output logic o{i}")).collect();
    let assigns: String = (0..12)
        .map(|i| format!("  assign o{i} = in.v;\n"))
        .collect();
    std::fs::write(
        &top,
        format!(
            "module w_top (input logic clk, w_if.sink in{outs});\n{assigns}  \
             wire [3:0] bad = 4'(nosuch_sig);\nendmodule\n"
        ),
    )
    .unwrap();
    let err = Dut::new("w_top")
        .sources([&iface, &top])
        .top("w_top")
        .bind_iface("in", "w_if.sink")
        .verilator_args(["-Wall"])
        .build_in(&s.env())
        .unwrap_err()
        .to_string();
    let first = err.lines().next().unwrap();
    assert!(
        first.contains("%Error") && first.contains("nosuch_sig"),
        "{first}\n{err}"
    );
    assert!(!err.contains("PINMISSING"), "{err}");
    assert!(!err.contains("hint: Verilator's lint warnings"), "{err}");
}
