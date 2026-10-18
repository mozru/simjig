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
    assert!(
        err.to_string()
            .contains(&format!("on `{}`", broken.display())),
        "{err}"
    );

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
             supported versions: 5.052; install the pinned one: `pixi install --locked`",
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
