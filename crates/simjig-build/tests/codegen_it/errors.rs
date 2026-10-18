//! Configuration mistakes and unsupported designs produce errors that name the item and
//! say what to do.

use simjig_build::codegen::{
    CodegenConfig, IfaceBinding, IfaceTop, Probe, ProbeAccess, ProbeType, lower, lower_wrapper,
};

use crate::cases;

fn probe(path: &str, module: &str, ty: ProbeType) -> CodegenConfig {
    CodegenConfig {
        probes: vec![Probe {
            path: path.into(),
            module: module.into(),
            ty,
            access: ProbeAccess::Read,
        }],
        ..CodegenConfig::default()
    }
}

fn bind(port: &str, interface: &str, modport: Option<&str>) -> IfaceBinding {
    IfaceBinding {
        port: port.into(),
        interface: interface.into(),
        params: Vec::new(),
        modport: modport.map(Into::into),
    }
}

fn iface(dut: &str, bindings: Vec<IfaceBinding>) -> IfaceTop {
    IfaceTop {
        dut: dut.into(),
        params: Vec::new(),
        bindings,
    }
}

#[test]
fn error_messages() {
    let structs = cases::fixture("structs").design;
    let modports = cases::fixture("interface_modports").design;
    let bits = |width| ProbeType::Bits { width };
    let mut out = Vec::new();
    for config in [
        probe("other.u.x", "top", bits(1)),
        probe("top.u.x", "nope", bits(1)),
        probe("top.x", "top", ProbeType::Named("pkg::nope_t".into())),
        probe("top.x", "top", bits(0)),
        probe("top.a$b", "top", bits(1)),
        CodegenConfig {
            probes: Vec::new(),
            iface_top: Some(iface("producer", Vec::new())),
        },
    ] {
        out.push(lower(&structs, &config).unwrap_err().to_string());
    }
    for config in [
        iface("producer", Vec::new()),
        iface("monitor", vec![bind("bus", "bus_if", None)]),
        iface("producer", vec![bind("bus", "other_if", None)]),
        iface("producer", vec![bind("bus", "bus_if", Some("snk"))]),
        iface(
            "producer",
            vec![bind("bus", "bus_if", None), bind("nope", "bus_if", None)],
        ),
        iface("absent", Vec::new()),
    ] {
        out.push(lower_wrapper(&modports, &config).unwrap_err().to_string());
    }
    insta::assert_snapshot!(out.join("\n"));
}

/// Each probe mistake is caught by the lowering, before verilation, with the item, what is
/// wrong and a fix: a mistyped instance or variable (with the closest name), a declaring
/// module that is not the instance's, and a read-write probe of a constant.
#[test]
fn probe_path_mistakes_name_the_fix() {
    let design = cases::probe_paths().design;
    let lower_with = |path: &str, module: &str, access| {
        let config = CodegenConfig {
            probes: vec![Probe {
                path: path.into(),
                module: module.into(),
                ty: ProbeType::Bits { width: 8 },
                access,
            }],
            ..CodegenConfig::default()
        };
        lower(&design, &config)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let (rd, rw) = (ProbeAccess::Read, ProbeAccess::ReadWrite);
    for (path, module, access, want) in [
        (
            "alu.u_mid.u_cre.result",
            "alu_core",
            rd,
            "codegen: probe alu.u_mid.u_cre.result: no instance `u_cre` in `alu.u_mid` (a \
             `mid`); did you mean `u_core`?",
        ),
        (
            "alu.u_mid.u_core.reslt",
            "alu_core",
            rd,
            "codegen: probe alu.u_mid.u_core.reslt: no variable `reslt` in `alu_core` (the \
             module of `alu.u_mid.u_core`); did you mean `result`?",
        ),
        (
            "alu.u_mid.u_core.result",
            "mid",
            rd,
            "codegen: probe alu.u_mid.u_core.result: `alu.u_mid.u_core` is an instance of \
             `alu_core`, not of `mid` as the probe declares; pass `\"alu_core\"` as the \
             probe's module",
        ),
        (
            "alu.LP",
            "alu",
            rw,
            "codegen: probe alu.LP: `LP` is a localparam of `alu`, a constant Verilator keeps \
             in read-only storage; probe it with `ProbeAccess::Read`",
        ),
        (
            "alu.P",
            "alu",
            rw,
            "codegen: probe alu.P: `P` is a parameter of `alu`, a constant Verilator keeps in \
             read-only storage; probe it with `ProbeAccess::Read`",
        ),
    ] {
        assert_eq!(lower_with(path, module, access), Err(want.to_owned()));
    }
    for (path, module, access) in [
        ("alu.u_mid.u_core.result", "alu_core", rw),
        ("alu.LP", "alu", rd),
        ("alu.P", "alu", rd),
        ("alu.acc", "alu", rw),
    ] {
        assert_eq!(lower_with(path, module, access), Ok(()), "{path}");
    }
}

/// Through the interface wrapper, the path is the DUT's, and so are the names checked.
#[test]
fn probe_paths_of_an_interface_ported_dut_are_checked_below_the_dut() {
    let case = cases::iface_top();
    let config = CodegenConfig {
        probes: vec![Probe {
            path: "iface_top.data_qq".into(),
            module: "iface_top".into(),
            ty: ProbeType::Bits { width: 8 },
            access: ProbeAccess::Read,
        }],
        ..case.config.clone()
    };
    let err = lower(&case.design, &config).unwrap_err().to_string();
    assert!(
        err.contains("no variable `data_qq` in `iface_top` (the module of `iface_top`); did you mean `data_q`?"),
        "{err}"
    );
}
