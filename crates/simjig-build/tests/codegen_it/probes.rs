//! Probes at run time, with the pinned Verilator: every case is lowered, emitted, built
//! with `verilator --build` around `probe_driver.cpp`, and then created, written and read
//! through the vtable.
//!
//! The non-inlined cases are the ones Verilator 5.052 keeps as separate module objects
//! (V3Inline.cpp): a module used more than once whose size times uses reaches
//! `--inline-mult`, a module that declares a class, and `/*verilator no_inline_module*/`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use simjig_build::codegen::{
    CodegenConfig, Lowered, Probe, ProbeAccess, ProbeType, ShimOptions, emit_shim, emit_vlt, lower,
};
use svtypes::verilator::{JsonPassOpts, json_pass};

use crate::cases::{self, Case};

/// A driver built for one case.
pub struct Driver {
    pub exe: PathBuf,
    pub build: Output,
}

impl Driver {
    /// Runs the driver with `ops` (see `probe_driver.cpp`); returns its stdout and whether
    /// it succeeded.
    pub fn run(&self, ops: &[&str]) -> (bool, String) {
        assert!(
            self.build.status.success(),
            "model build failed:\n{}",
            String::from_utf8_lossy(&self.build.stderr)
        );
        let out = Command::new(&self.exe).args(ops).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// Runs `ops` and expects success.
    pub fn check(&self, ops: &[&str]) {
        let (ok, stdout) = self.run(ops);
        assert!(ok && stdout.ends_with("ok\n"), "driver failed:\n{stdout}");
    }
}

/// Emits the shim and `.vlt` of `lowered` and builds them with `case`'s sources, the
/// generic driver and the extra Verilator `flags`.
pub fn build_driver(case: &Case, lowered: &Lowered, opts: &ShimOptions, flags: &[&str]) -> Driver {
    let dir = cases::scratch(&format!("driver_{}", case.name));
    let shim = dir.join(format!("{}_shim.cpp", opts.prefix));
    fs::write(&shim, emit_shim(lowered, opts).unwrap()).unwrap();
    let vlt = dir.join("probes.vlt");
    fs::write(&vlt, emit_vlt(lowered)).unwrap();
    let cpp = Path::new(env!("CARGO_MANIFEST_DIR")).join("cpp");
    let cflags = format!(
        "-include {0}/simjig_vl_config.h -I{0} -std=gnu++20 -DSIMJIG_VT=simjig_vtable_{1}",
        cpp.display(),
        opts.prefix
    );
    let mdir = dir.join("obj");
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/codegen_it/probe_driver.cpp");
    let mut files = vec![vlt];
    files.extend(case.sources.iter().cloned());
    files.extend([shim, driver, cpp.join("simjig_rt.cpp")]);
    let mut args = vec![
        "--cc",
        "--exe",
        "--build",
        "-j",
        "4",
        "--no-timing",
        "-Wno-fatal",
        "--top-module",
        &lowered.top,
        "--prefix",
        &opts.prefix,
        "--Mdir",
        mdir.to_str().unwrap(),
        "-CFLAGS",
        &cflags,
    ];
    args.extend_from_slice(flags);
    let build = Command::new("verilator")
        .args(&args)
        .args(&files)
        .output()
        .expect("verilator on PATH (run under `pixi run`)");
    Driver {
        exe: mdir.join(&opts.prefix),
        build,
    }
}

pub fn opts(prefix: &str) -> ShimOptions {
    ShimOptions {
        prefix: prefix.into(),
        timing: false,
        trace_fst: false,
        coverage: false,
    }
}

fn rw_probe(path: &str, module: &str, width: u32) -> Probe {
    Probe {
        path: path.into(),
        module: module.into(),
        ty: ProbeType::Bits { width },
        access: ProbeAccess::ReadWrite,
    }
}

/// A case from SystemVerilog text written to the scratch directory, top module `top`.
fn sv_case(name: &str, text: &str, probes: Vec<Probe>) -> Case {
    let dir = cases::scratch(&format!("sv_{name}"));
    let file = dir.join("design.sv");
    fs::write(&file, text).unwrap();
    let sources = vec![file];
    let design = json_pass(&JsonPassOpts {
        sources: sources.clone(),
        top: "top".into(),
        ..JsonPassOpts::default()
    })
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    Case {
        name: name.into(),
        design,
        config: CodegenConfig {
            probes,
            ..CodegenConfig::default()
        },
        sources,
    }
}

/// `leaf` with `body` inside, instantiated `uses` times by `top` (`u0`, `u1`, ...), whose
/// `q<i>` is the instance's `x` (8 bits) XOR `pad` (0 until a clock edge).
fn leaf_design(body: &str, uses: usize) -> String {
    let mut sv = format!(
        "module leaf(input logic clk, input logic [7:0] d, output logic [7:0] q);\n{body}\n\
         endmodule\nmodule top(input logic clk, input logic [7:0] d"
    );
    for i in 0..uses {
        write!(sv, ", output logic [7:0] q{i}").unwrap();
    }
    sv.push_str(");\n");
    for i in 0..uses {
        writeln!(sv, "  leaf u{i}(.clk, .d, .q(q{i}));").unwrap();
    }
    sv + "endmodule\n"
}

const SMALL_LEAF: &str = "  logic [7:0] x;\n  always_ff @(posedge clk) x <= x + d;\n  \
                          assign q = x;";

/// Builds `case` (top `top`), checks that Verilator kept `leaf` as its own class, and
/// writes and reads `top.u<i>.x` through the probes of every instance.
fn check_leaf_probes(case: &Case, uses: usize) {
    let lowered = lower(&case.design, &case.config).unwrap();
    let prefix = format!("V{}", case.name);
    let driver = build_driver(case, &lowered, &opts(&prefix), &[]);
    let (ok, _) = driver.run(&[]);
    let leaf_class = driver
        .exe
        .parent()
        .unwrap()
        .join(format!("{prefix}_leaf.h"));
    assert!(ok && leaf_class.exists(), "{}: leaf was inlined", case.name);
    let mut ops: Vec<String> = Vec::new();
    for i in 0..uses {
        ops.push(format!("w:top.u{i}.x:{:x}", 0x30 + i));
    }
    ops.push("e".into());
    for i in 0..uses {
        ops.push(format!("r:top.u{i}.x:{:x}", 0x30 + i));
        ops.push(format!("r:top.q{i}:{:x}", 0x30 + i));
    }
    let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
    driver.check(&ops);
    // Scopes are named after the instance the testbench gives the model.
    let mut named = vec!["name=dut0"];
    named.extend(&ops);
    driver.check(&named);
}

fn leaf_probes(uses: usize) -> Vec<Probe> {
    (0..uses)
        .map(|i| rw_probe(&format!("top.u{i}.x"), "leaf", 8))
        .collect()
}

/// 300 statements, used three times: 5.052 inlines a module used more than once only
/// while its size times its uses stays below `--inline-mult` (2000).
#[test]
fn probes_work_in_a_large_module_used_three_times() {
    let mut body = String::from("  logic [7:0] x;\n  logic [7:0] pad;\n");
    for i in 0..300 {
        writeln!(body, "  logic [7:0] r{i};").unwrap();
    }
    body.push_str("  always_ff @(posedge clk) begin\n");
    for i in 0..300 {
        let prev = if i == 0 {
            "d".to_owned()
        } else {
            format!("r{}", i - 1)
        };
        writeln!(body, "    r{i} <= {prev} ^ d;").unwrap();
    }
    body.push_str("    pad <= r299;\n  end\n  assign q = x ^ pad;");
    let case = sv_case("inline_big", &leaf_design(&body, 3), leaf_probes(3));
    check_leaf_probes(&case, 3);
}

/// A module that declares a class is never inlined.
#[test]
fn probes_work_in_a_module_that_declares_a_class() {
    let body = format!(
        "  class C; int v; function int get(); return v; endfunction endclass\n{SMALL_LEAF}"
    );
    let case = sv_case("inline_class", &leaf_design(&body, 1), leaf_probes(1));
    check_leaf_probes(&case, 1);
}

/// `/*verilator no_inline_module*/` in the source, which no flag check can see.
#[test]
fn probes_work_in_a_no_inline_module() {
    let body = format!("  /*verilator no_inline_module*/\n{SMALL_LEAF}");
    let case = sv_case("inline_meta", &leaf_design(&body, 2), leaf_probes(2));
    check_leaf_probes(&case, 2);
}

/// `-fno-inline` used to be rejected for designs with probes; they now work with it.
#[test]
fn probes_work_with_inlining_off() {
    let case = sv_case("inline_off", &leaf_design(SMALL_LEAF, 2), leaf_probes(2));
    let lowered = lower(&case.design, &case.config).unwrap();
    let driver = build_driver(&case, &lowered, &opts("Vinline_off"), &["-fno-inline"]);
    driver.check(&[
        "w:top.u1.x:7",
        "e",
        "r:top.q1:7",
        "r:top.u1.x:7",
        "r:top.q0:0",
    ]);
}

/// A hierarchical block is a separate Verilated model whose variables the top's scope
/// table does not hold: create fails and says why, instead of the C++ build failing.
#[test]
fn a_probe_inside_a_hierarchical_block_fails_create_with_the_reason() {
    let body = format!("  /*verilator hier_block*/\n{SMALL_LEAF}");
    let case = sv_case("inline_hier", &leaf_design(&body, 1), leaf_probes(1));
    let lowered = lower(&case.design, &case.config).unwrap();
    let driver = build_driver(&case, &lowered, &opts("Vinline_hier"), &["--hierarchical"]);
    let (ok, stdout) = driver.run(&[]);
    assert!(!ok, "{stdout}");
    assert!(
        stdout.contains("probe 'top.u0.x': no public variable 'x' in scope 'TOP.top.u0'")
            && stdout.contains("hier_block"),
        "{stdout}"
    );
}

/// A probe of an interface-ported DUT is spelled from the DUT (`iface_top.data_q`) but
/// lives below the wrapper Verilator elaborates as top (`<wrapper>.dut.data_q`).
#[test]
fn probes_of_an_interface_ported_dut_bind_at_create() {
    let mut case = cases::iface_top();
    case.config.probes = vec![rw_probe("iface_top.data_q", "iface_top", 16)];
    let lowered = lower(&case.design, &case.config).unwrap();
    let driver = build_driver(&case, &lowered, &opts("Viface_probe"), &[]);
    driver.check(&[
        "w:iface_top.data_q:beef",
        "e",
        "r:iface_top.data_q:beef",
        "r:simjig_wrap_iface_top.out_bus_data:beef",
    ]);
}

/// Probes through generate blocks bind at create: a named and an unnamed generate-if
/// (`genblk2`, IEEE 1800-2023 §27.6), a generate-for iteration (`g[1]`), and a variable of
/// the loop body, which belongs to the enclosing module.
#[test]
fn probes_through_generate_blocks_bind_at_create() {
    let sv = format!(
        "module leaf(input logic clk, input logic [7:0] d, output logic [7:0] q);\n\
         {SMALL_LEAF}\nendmodule\n\
         module top(input logic clk, input logic [7:0] d, output logic [7:0] q0,\n\
         output logic [7:0] q1, output logic [7:0] q2);\n\
         if (1) begin : g_x\n leaf u(.clk, .d, .q(q0));\n end\n\
         if (1) begin\n leaf u(.clk, .d, .q(q1));\n end\n\
         for (genvar i = 0; i < 2; i++) begin : g\n logic [7:0] t;\n\
         if (i == 1) begin : one\n leaf u(.clk, .d, .q(q2));\n end\n end\n\
         endmodule\n"
    );
    let probes = vec![
        rw_probe("top.g_x.u.x", "leaf", 8),
        rw_probe("top.genblk2.u.x", "leaf", 8),
        rw_probe("top.g[1].one.u.x", "leaf", 8),
        rw_probe("top.g[0].t", "top", 8),
    ];
    let case = sv_case("generate", &sv, probes);
    let lowered = lower(&case.design, &case.config).unwrap();
    let driver = build_driver(&case, &lowered, &opts("Vgenerate"), &[]);
    driver.check(&[
        "w:top.g_x.u.x:11",
        "w:top.genblk2.u.x:22",
        "w:top.g[1].one.u.x:33",
        "w:top.g[0].t:44",
        "e",
        "r:top.q0:11",
        "r:top.q1:22",
        "r:top.q2:33",
        "r:top.g[0].t:44",
    ]);
}
