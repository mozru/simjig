//! The designs the codegen tests run on, each with its configuration and sources.
//!
//! - every svtypes fixture, decoded from its committed Verilator 5.052 output;
//! - `tests/duts/struct_pipe` and `tests/duts/iface_top` (through the generated wrapper),
//!   and `cases/wide`, elaborated with the Verilator on `PATH` (`pixi run`).

use std::fs;
use std::path::{Path, PathBuf};

use simjig_build::codegen::{
    CodegenConfig, IfaceBinding, IfaceTop, Probe, ProbeAccess, ProbeType, emit_elab_top,
    emit_wrapper, lower_wrapper,
};
use svtypes::ir::Design;
use svtypes::verilator::{JsonPassOpts, json_pass};

/// One design with everything the emitters and Verilator need.
#[derive(Debug)]
pub struct Case {
    pub name: String,
    pub design: Design,
    pub config: CodegenConfig,
    /// Sources in compile order, including the generated wrapper if there is one.
    pub sources: Vec<PathBuf>,
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo() -> PathBuf {
    manifest().join("../..")
}

/// A fresh scratch directory for `name` under Cargo's per-target temp dir. nextest runs
/// each test in its own process, concurrently, so the directory is per process.
pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("codegen")
        .join(format!("{name}-{}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The names of the svtypes fixtures, sorted.
pub fn fixture_names() -> Vec<String> {
    let root = manifest().join("../svtypes/tests/fixtures");
    let mut names: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.len() >= 10, "svtypes fixture corpus is missing cases");
    names
}

/// An svtypes fixture, from its committed JSON.
pub fn fixture(name: &str) -> Case {
    let dir = manifest().join("../svtypes/tests/fixtures").join(name);
    let read = |f: &str| fs::read_to_string(dir.join(f)).unwrap();
    let design = svtypes::frontend::parse(
        &read("tree.json"),
        &read("tree.meta.json"),
        &read("verilator-version"),
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    Case {
        name: name.to_owned(),
        design,
        config: CodegenConfig::default(),
        sources: vec![dir.join("design.sv")],
    }
}

fn pass(sources: &[PathBuf], top: &str) -> Design {
    json_pass(&JsonPassOpts {
        sources: sources.to_vec(),
        top: top.to_owned(),
        ..JsonPassOpts::default()
    })
    .unwrap_or_else(|e| panic!("{top}: {e}"))
}

/// `tests/duts/struct_pipe`.
pub fn struct_pipe() -> Case {
    let dir = repo().join("tests/duts/struct_pipe");
    let sources = vec![dir.join("struct_pipe_pkg.sv"), dir.join("struct_pipe.sv")];
    Case {
        name: "struct_pipe".into(),
        design: pass(&sources, "struct_pipe"),
        config: CodegenConfig::default(),
        sources,
    }
}

/// The interface configuration of `tests/duts/iface_top`.
pub fn iface_top_config() -> IfaceTop {
    let bind = |port: &str| IfaceBinding {
        port: port.into(),
        interface: "bus_if".into(),
        params: Vec::new(),
        modport: None,
    };
    IfaceTop {
        dut: "iface_top".into(),
        params: Vec::new(),
        bindings: vec![bind("in_bus"), bind("out_bus")],
    }
}

/// Elaborates an interface-ported DUT the way the build does: the JSON pass under the
/// elaboration top, the wrapper from that IR, then the JSON pass under the wrapper.
pub fn through_wrapper(name: &str, sources: Vec<PathBuf>, iface: IfaceTop) -> Case {
    let dir = scratch(name);
    let elab = dir.join("elab.sv");
    fs::write(&elab, emit_elab_top(&iface)).unwrap();
    let mut elab_sources = sources.clone();
    elab_sources.push(elab);
    let first = pass(
        &elab_sources,
        &simjig_build::codegen::elab_module(&iface.dut),
    );
    let wrapper = lower_wrapper(&first, &iface).unwrap();
    let wrap = dir.join(format!("{}.sv", wrapper.module));
    fs::write(&wrap, emit_wrapper(&wrapper)).unwrap();
    let mut sources = sources;
    sources.push(wrap);
    Case {
        name: name.to_owned(),
        design: pass(&sources, &wrapper.module),
        config: CodegenConfig {
            probes: Vec::new(),
            iface_top: Some(iface),
        },
        sources,
    }
}

/// `tests/duts/iface_top`, through the generated wrapper.
pub fn iface_top() -> Case {
    let dir = repo().join("tests/duts/iface_top");
    let sources = ["iface_pkg.sv", "bus_if.sv", "iface_top.sv"]
        .map(|f| dir.join(f))
        .to_vec();
    through_wrapper("iface_top", sources, iface_top_config())
}

/// `producer` of the svtypes `interface_modports` fixture: a parameterised interface with
/// an interface port (`clk`) of its own.
pub fn producer() -> Case {
    let source = manifest().join("../svtypes/tests/fixtures/interface_modports/design.sv");
    let iface = IfaceTop {
        dut: "producer".into(),
        params: Vec::new(),
        bindings: vec![IfaceBinding {
            port: "bus".into(),
            interface: "bus_if".into(),
            params: vec![("W".into(), "16".into())],
            modport: None,
        }],
    };
    through_wrapper("producer", vec![source], iface)
}

/// The probes of `cases/wide`.
pub fn wide_probes() -> Vec<Probe> {
    vec![
        Probe {
            path: "wide_top.state".into(),
            module: "wide_top".into(),
            ty: ProbeType::Named("wide_pkg::mode_e".into()),
            access: ProbeAccess::Read,
        },
        Probe {
            path: "wide_top.knob".into(),
            module: "wide_top".into(),
            ty: ProbeType::Bits { width: 8 },
            access: ProbeAccess::ReadWrite,
        },
        Probe {
            path: "wide_top.u_acc.sum".into(),
            module: "acc".into(),
            ty: ProbeType::Bits { width: 8 },
            access: ProbeAccess::Read,
        },
    ]
}

/// `cases/wide`, with probes.
pub fn wide() -> Case {
    let sources = vec![manifest().join("tests/codegen_it/cases/wide/design.sv")];
    Case {
        name: "wide".into(),
        design: pass(&sources, "wide_top"),
        config: CodegenConfig {
            probes: wide_probes(),
            iface_top: None,
        },
        sources,
    }
}

/// `cases/nested_anon`: anonymous structs and unions nested in a package struct and in an
/// anonymous struct port. Not in [`all`]: only the layout check is about it.
pub fn nested_anon() -> Case {
    let sources = vec![manifest().join("tests/codegen_it/cases/nested_anon/design.sv")];
    Case {
        name: "nested_anon".into(),
        design: pass(&sources, "nested_top"),
        config: CodegenConfig::default(),
        sources,
    }
}

/// `cases/iface_param`: a parameterised interface-ported DUT, `K = 3`, through the wrapper.
/// Not in [`all`]: the parameter and probe tests use it.
pub fn iface_param() -> Case {
    let sources = vec![manifest().join("tests/codegen_it/cases/iface_param/design.sv")];
    let iface = IfaceTop {
        dut: "r_dut".into(),
        params: vec![("K".into(), "3".into())],
        bindings: vec![IfaceBinding {
            port: "bus".into(),
            interface: "r_if".into(),
            params: Vec::new(),
            modport: None,
        }],
    };
    through_wrapper("iface_param", sources, iface)
}

/// `cases/probe_paths`: `alu.u_mid.u_core` with a parameter and a localparam in `alu`. Not
/// in [`all`]: only the probe path errors use it.
pub fn probe_paths() -> Case {
    let sources = vec![manifest().join("tests/codegen_it/cases/probe_paths/design.sv")];
    Case {
        name: "probe_paths".into(),
        design: pass(&sources, "alu"),
        config: CodegenConfig::default(),
        sources,
    }
}

/// Every case: the fixtures, then the Verilator-elaborated ones.
pub fn all() -> Vec<Case> {
    let mut cases: Vec<Case> = fixture_names().iter().map(|n| fixture(n)).collect();
    cases.extend([struct_pipe(), iface_top(), producer(), wide()]);
    cases
}
