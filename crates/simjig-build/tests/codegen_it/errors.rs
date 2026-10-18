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
            ..CodegenConfig::default()
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

/// Probes name root members that exist only when every module is inlined into the root
/// (`top__DOT__u__DOT__x`). Flags and control files that keep modules apart are rejected
/// with the reason, not left to fail in the C++ compiler.
#[test]
fn probes_reject_builds_that_do_not_inline() {
    let design = cases::wide().design;
    let with = |args: &[&str], vlt: &[&str]| CodegenConfig {
        probes: cases::wide_probes(),
        verilator_args: args.iter().map(|a| (*a).to_owned()).collect(),
        control_files: vlt.iter().map(|v| (*v).to_owned()).collect(),
        iface_top: None,
    };
    let no_inline_acc = "`verilator_config\nno_inline -module \"acc\"\n";
    let hier_acc = "`verilator_config\n// a comment\nhier_block -module \"acc\"\n";
    let mut out = Vec::new();
    for config in [
        with(&["-fno-inline"], &[]),
        with(&["-O2", "--O0"], &[]),
        with(&["--hierarchical"], &[]),
        with(&[], &[no_inline_acc]),
        with(&[], &[hier_acc]),
    ] {
        out.push(lower(&design, &config).unwrap_err().to_string());
    }
    insta::assert_snapshot!(out.join("\n"));
    for ok in [
        with(&["-O0", "-finline"], &[]),
        with(&["--hierarchical", "--no-hierarchical"], &[]),
        with(
            &[],
            &["`verilator_config\nno_inline -module \"acc\" -task \"t\"\n"],
        ),
        CodegenConfig {
            probes: Vec::new(),
            ..with(&["-fno-inline"], &[no_inline_acc])
        },
    ] {
        assert!(lower(&design, &ok).is_ok(), "{ok:?}");
    }
}
