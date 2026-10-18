//! The emitter seam with the real code generator: what it returns reaches Verilator, the
//! compiler and `OUT_DIR`, and the interface-port flow makes the generated wrapper the top.

use std::path::Path;
use std::process::Command;

use simjig_build::svtypes::ir::Design;
use simjig_build::svtypes::verilator::{JsonPassOpts, json_pass};
use simjig_build::{
    Built, Dut, DutConfig, Emitted, Error, ProbeAccess, ProbeType, SvModule, codegen_emitter,
    ir_ports,
};

use crate::support::{Sandbox, dut};

/// The model archive of `built` (in the shared cache, named in its link directives)
/// exports its vtable symbol.
fn exports_vtable(cache: &Path, built: &Built) -> bool {
    let name = format!("libsimjig_model_{}_", built.prefix);
    let archive = built
        .directives
        .iter()
        .filter_map(|d| d.strip_prefix("cargo::rustc-link-arg="))
        .map(Path::new)
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&name))
        })
        .unwrap_or_else(|| panic!("no model archive in {:?}", built.directives));
    assert!(
        archive.starts_with(cache.join("archives")),
        "{}",
        archive.display()
    );
    let nm = Command::new("nm").arg(archive).output().unwrap();
    String::from_utf8_lossy(&nm.stdout).contains(&format!("simjig_vtable_{}", built.prefix))
}

#[test]
fn generated_files_reach_out_dir_verilator_and_the_compiler() {
    let s = Sandbox::new();
    let env = s.env();
    let d = dut("struct_pipe");
    let built = Dut::new("struct_pipe")
        .sources([d.join("struct_pipe_pkg.sv"), d.join("struct_pipe.sv")])
        .top("struct_pipe")
        .probe(
            "struct_pipe.s1_valid",
            "struct_pipe",
            ProbeType::Bits { width: 1 },
            ProbeAccess::Read,
        )
        .build_in(&env)
        .unwrap();
    let rust = std::fs::read_to_string(built.rust_file.as_deref().unwrap()).unwrap();
    assert_eq!(
        built.rust_file.as_deref(),
        Some(env.out_dir.join("simjig/struct_pipe.rs").as_path())
    );
    assert!(rust.contains("pub struct Dut"), "{rust}");
    assert!(
        rust.contains("pub struct Beat"),
        "the package struct: {rust}"
    );
    // The shim is content-addressed in the cache's `inputs/<hash>/`.
    let shim_name = format!("{}_shim.cpp", built.prefix);
    let shim = std::fs::read_dir(env.cache_dir().join("inputs"))
        .unwrap()
        .map(|e| e.unwrap().path().join(&shim_name))
        .find(|p| p.is_file())
        .expect("the shim in the cache");
    let shim = std::fs::read_to_string(shim).unwrap();
    // The probe is found at create in the scope table that the `.vlt` fills.
    assert!(
        shim.contains(r#"SIMJIG_PROBE(t, m, "struct_pipe", "s1_valid""#),
        "{shim}"
    );
    assert!(exports_vtable(&env.cache_dir(), &built));
}

#[test]
fn an_interface_ported_dut_is_built_through_the_generated_wrapper() {
    let s = Sandbox::new();
    let env = s.env();
    let d = dut("iface_top");
    let built = Dut::new("iface_top")
        .sources([
            d.join("iface_pkg.sv"),
            d.join("bus_if.sv"),
            d.join("iface_top.sv"),
        ])
        .top("iface_top")
        .bind_iface("in_bus", "bus_if")
        .bind_iface("out_bus", "bus_if")
        .build_in(&env)
        .unwrap();
    assert_eq!(built.design.top_module().name, "simjig_wrap_iface_top");
    let names: Vec<&str> = built.header_ports.iter().map(|p| p.name.as_str()).collect();
    assert!(
        names.contains(&"in_bus_valid") && names.contains(&"out_bus_data"),
        "{names:?}"
    );
    assert!(exports_vtable(&env.cache_dir(), &built));
}

/// Every unit declaring a `timeunit`, as the README recommends, needs no `--timescale`:
/// the generated tops declare none, and Verilator 5.052 would stop on `TIMESCALEMOD` for
/// them (IEEE 1800-2023 §3.14.2.3) unless they waive it and inherit the design's unit.
#[test]
fn an_interface_ported_dut_whose_units_all_declare_a_timeunit_builds() {
    let s = Sandbox::new();
    let src = dut("iface_top");
    let timed = tempfile::tempdir().unwrap();
    let unit = "  timeunit 1ns; timeprecision 1ps;";
    for (file, after) in [
        ("iface_pkg.sv", "package iface_pkg;"),
        ("bus_if.sv", "interface bus_if;"),
        ("iface_top.sv", ");"),
    ] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        let (head, tail) = text.split_once(&format!("\n{after}\n")).unwrap();
        let text = format!("{head}\n{after}\n{unit}\n{tail}");
        std::fs::write(timed.path().join(file), text).unwrap();
    }
    let d = timed.path();
    let built = Dut::new("iface_timed")
        .sources([
            d.join("iface_pkg.sv"),
            d.join("bus_if.sv"),
            d.join("iface_top.sv"),
        ])
        .top("iface_top")
        .bind_iface("in_bus", "bus_if")
        .bind_iface("out_bus", "bus_if")
        .build_in(&s.env())
        .unwrap();
    assert_eq!(built.design.time_precision.to_string(), "1ps");
}

fn failing_layout(design: &Design, config: &DutConfig) -> Result<Emitted, Error> {
    let mut e = codegen_emitter(design, config)?;
    e.layout_check = Some(SvModule {
        module: "bad_layout".into(),
        contents: "module bad_layout;\n  if ($bits(logic [3:0]) != 5) \
                   $fatal(1, \"layout of x differs\");\nendmodule\n"
            .into(),
    });
    Ok(e)
}

#[test]
fn a_failing_layout_check_stops_the_build_before_verilating() {
    let s = Sandbox::new();
    let err = Dut::new("inv")
        .sources([dut("inv").join("inv.sv")])
        .top("inv")
        .emitter(failing_layout)
        .build_in(&s.env())
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, Error::Command { .. }) && msg.contains("layout of x differs"),
        "{msg}"
    );
    assert!(msg.contains("-Werror-USERFATAL"), "{msg}");
    assert_eq!(s.verilations(), 0);
}

#[test]
fn codegen_errors_name_the_dut() {
    let s = Sandbox::new();
    let err = Dut::new("inv")
        .sources([dut("inv").join("inv.sv")])
        .top("inv")
        .probe(
            "inv.nope",
            "inv",
            ProbeType::Named("no::such_t".into()),
            ProbeAccess::Read,
        )
        .build_in(&s.env())
        .unwrap_err();
    // A probe mistake reads like the probe checks after verilating (`Error::Config`).
    assert!(matches!(err, Error::Config { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "invalid DUT `inv` configuration: probe `inv.nope` (declared in build.rs): no type \
         named `no::such_t` in the IR"
    );
}

/// The header is checked against the IR's ports; also compare them here, sorted.
fn ir_matches_header(s: &Sandbox, name: &str, sources: &[&Path], top: &str) {
    let built = Dut::new(name)
        .sources(sources)
        .top(top)
        .build_in(&s.env())
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut want = ir_ports(&built.design).unwrap();
    let mut got = built.header_ports.clone();
    want.sort();
    got.sort();
    assert_eq!(got, want, "{name}");
}

#[test]
fn ir_ports_match_the_header() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    // Every storage class and direction, a `[5:2]` range and an `inout`.
    ir_matches_header(&s, "ports", &[&rtl.join("ports.sv")], "ports");
    let d = dut("struct_pipe");
    let pkg = d.join("struct_pipe_pkg.sv");
    ir_matches_header(
        &s,
        "struct_pipe",
        &[&pkg, &d.join("struct_pipe.sv")],
        "struct_pipe",
    );
    ir_matches_header(&s, "wide", &[&dut("wide").join("wide.sv")], "wide");
}

/// Storage comes from `svtypes::width::packed_storage`, the one storage rule, so a
/// zero-width port is an error here too rather than a made-up `CData`.
#[test]
fn ir_ports_rejects_a_zero_width_port() {
    let rtl = crate::support::fixtures().join("rtl");
    let mut design = json_pass(&JsonPassOpts {
        sources: vec![rtl.join("counter.sv")],
        include_dirs: vec![rtl],
        top: "counter".into(),
        ..JsonPassOpts::default()
    })
    .unwrap();
    let count = design.top_module().ports[2].ty;
    design.types[count.index()].width = Some(0);
    let err = ir_ports(&design).unwrap_err().to_string();
    assert!(
        err.contains("`count`") && err.contains("zero-width"),
        "{err}"
    );
}

/// The shim's `kCoverage` is the coverage Verilator builds: a coverage kind among the
/// extra arguments turns it on, an option that only tunes coverage does not (its model has
/// no counters and its link would need `verilated_cov.cpp` for nothing).
#[test]
fn shim_coverage_follows_the_extra_arguments() {
    let rtl = crate::support::fixtures().join("rtl");
    let design = json_pass(&JsonPassOpts {
        sources: vec![rtl.join("counter.sv")],
        include_dirs: vec![rtl.clone()],
        top: "counter".into(),
        ..JsonPassOpts::default()
    })
    .unwrap();
    let shim = |args: &[&str]| {
        let config = Dut::new("counter")
            .sources([rtl.join("counter.sv")])
            .top("counter")
            .verilator_args(args.iter().copied())
            .config()
            .clone();
        codegen_emitter(&design, &config).unwrap().shim_cpp.unwrap()
    };
    assert!(shim(&["--coverage-line"]).contains("kCoverage = true;"));
    assert!(shim(&["--coverage-max-width", "8"]).contains("kCoverage = false;"));
    assert!(shim(&["--coverage", "--no-coverage"]).contains("kCoverage = false;"));
}

/// s3c7m: the waived wrapper must not change the design's precision, whatever the user's.
#[test]
fn s3c7m_the_waived_wrapper_keeps_the_users_precision() {
    for (unit, want) in [
        ("  timeunit 1ns; timeprecision 1ns;", "1ns"),
        ("  timeunit 1us; timeprecision 100ps;", "100ps"),
        ("  timeunit 10ns; timeprecision 10ns;", "10ns"),
    ] {
        let s = Sandbox::new();
        let src = dut("iface_top");
        let timed = tempfile::tempdir().unwrap();
        for (file, after) in [
            ("iface_pkg.sv", "package iface_pkg;"),
            ("bus_if.sv", "interface bus_if;"),
            ("iface_top.sv", ");"),
        ] {
            let text = std::fs::read_to_string(src.join(file)).unwrap();
            let (head, tail) = text.split_once(&format!("\n{after}\n")).unwrap();
            let text = format!("{head}\n{after}\n{unit}\n{tail}");
            std::fs::write(timed.path().join(file), text).unwrap();
        }
        let d = timed.path();
        let built = Dut::new("iface_timed")
            .sources([
                d.join("iface_pkg.sv"),
                d.join("bus_if.sv"),
                d.join("iface_top.sv"),
            ])
            .top("iface_top")
            .bind_iface("in_bus", "bus_if")
            .bind_iface("out_bus", "bus_if")
            .build_in(&s.env())
            .unwrap();
        assert_eq!(built.design.time_precision.to_string(), want, "{unit}");
    }
}
