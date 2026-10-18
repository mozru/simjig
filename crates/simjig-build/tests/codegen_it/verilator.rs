//! End-to-end checks with the pinned Verilator (`pixi run`): the layout check elaborates
//! cleanly and catches a wrong IR, the wrapper lints cleanly and matches its plan, and the
//! C++ shim builds against a real model and exports a working vtable.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use simjig_build::codegen::{
    Access, ItemKind, LAYOUT_CHECK_FLAGS, Lowered, ShimOptions, emit_layout_check, emit_shim,
    emit_vlt, emit_wrapper, lower,
};

use crate::cases::{self, Case};

fn verilator(args: &[&str], files: &[PathBuf]) -> Output {
    Command::new("verilator")
        .args(args)
        .args(files)
        .output()
        .expect("verilator on PATH (run under `pixi run`)")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Lints `lowered`'s layout check against the case sources; returns the output.
fn run_layout_check(case: &Case, lowered: &Lowered) -> Output {
    let dir = cases::scratch(&format!("layout_{}", case.name));
    let file = dir.join(format!("{}.sv", lowered.layout.module));
    fs::write(&file, emit_layout_check(lowered)).unwrap();
    let mut files = case.sources.clone();
    files.push(file);
    let mut args: Vec<&str> = LAYOUT_CHECK_FLAGS.to_vec();
    args.extend([
        "-Wall",
        "-Wno-fatal",
        "--top-module",
        &lowered.layout.module,
    ]);
    verilator(&args, &files)
}

#[test]
fn layout_checks_pass_and_are_lint_clean() {
    for case in cases::all() {
        let lowered = lower(&case.design, &case.config).unwrap();
        let out = run_layout_check(&case, &lowered);
        let err = stderr(&out);
        assert!(
            out.status.success(),
            "{}: layout check failed:\n{err}",
            case.name
        );
        let ours = format!("{}.sv", lowered.layout.module);
        let warnings: Vec<&str> = err
            .lines()
            .filter(|l| l.starts_with('%') && l.contains(&ours))
            .collect();
        assert!(
            warnings.is_empty(),
            "{}: warnings in the layout check:\n{err}",
            case.name
        );
    }
}

/// Damages a lowering the way a wrong IR would.
type Corrupt = fn(&mut Lowered);

#[test]
fn layout_check_catches_a_wrong_ir() {
    let case = cases::fixture("structs");
    let good = lower(&case.design, &case.config).unwrap();
    let breakages: [(&str, Corrupt); 3] = [
        ("member tag of pkg::inner_t is not bits [14:11]", |l| {
            l.layout.checks[1].members[0].lsb = 11;
        }),
        ("$bits(pkg::outer_t) differs from the IR width 23", |l| {
            l.layout.checks[2].width = 23;
        }),
        ("pkg::BUSY differs from the IR value 2'h2", |l| {
            l.layout.checks[0].values[1].literal = "2'h2".into();
        }),
    ];
    for (expected, corrupt) in breakages {
        let mut bad = good.clone();
        corrupt(&mut bad);
        let out = run_layout_check(&case, &bad);
        let err = stderr(&out);
        assert!(!out.status.success(), "{expected}: the check passed");
        assert!(
            err.contains("%Error-USERFATAL") && err.contains(expected),
            "{expected}:\n{err}"
        );
    }
}

#[test]
fn wrappers_lint_clean_and_match_their_plan() {
    for case in [cases::iface_top(), cases::producer()] {
        let lowered = lower(&case.design, &case.config).unwrap();
        let wrapper = lowered.wrapper.as_ref().unwrap();
        // The plan from the wrapper-top IR reproduces the file elaborated from the
        // elaboration-top IR.
        let written = case.sources.last().unwrap();
        assert_eq!(
            fs::read_to_string(written).unwrap(),
            emit_wrapper(wrapper),
            "{}",
            case.name
        );
        let out = verilator(
            &["--lint-only", "-Wall", "--top-module", &wrapper.module],
            &case.sources,
        );
        let err = stderr(&out);
        let file = written.file_name().unwrap().to_string_lossy().into_owned();
        let ours: Vec<&str> = err
            .lines()
            .filter(|l| l.starts_with('%') && l.contains(&file))
            .collect();
        assert!(ours.is_empty(), "{}: wrapper warnings:\n{err}", case.name);
    }
}

#[test]
fn interface_ports_of_the_interface_become_bundle_inputs() {
    let lowered = {
        let case = cases::producer();
        lower(&case.design, &case.config).unwrap()
    };
    let bundle = &lowered.bundles[0];
    let members: Vec<(&str, Access)> = bundle
        .members
        .iter()
        .map(|m| (m.sv_name.as_str(), m.access))
        .collect();
    assert_eq!(
        members,
        [
            ("data", Access::Out),
            ("valid", Access::Out),
            ("ready", Access::In),
            ("clk", Access::In)
        ]
    );
    let iface = &lowered.wrapper.as_ref().unwrap().ifaces[0];
    assert_eq!(iface.port_conns, [("clk".to_owned(), "bus_clk".to_owned())]);
    assert!(
        iface
            .assigns
            .iter()
            .all(|(l, r)| !l.contains("clk") && !r.contains("clk"))
    );
}

/// `(name, width, storage, dir, access)` per slot, as the driver prints them.
fn expected_table(lowered: &Lowered) -> String {
    let mut out = String::new();
    for s in &lowered.signals {
        let storage = match s.width {
            1..=8 => 1,
            9..=16 => 2,
            17..=32 => 3,
            33..=64 => 4,
            _ => 5,
        };
        let (dir, access) = match s.access {
            Access::In => (1, 3),
            Access::Out => (2, 1),
            Access::InOut => (3, 3),
            Access::PublicRd => (0, 1),
            Access::PublicRw => (0, 3),
        };
        writeln!(
            out,
            "{} {} {} {storage} {dir} {access}",
            s.slot, s.name, s.width
        )
        .unwrap();
    }
    out + "ok\n"
}

fn cpp_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("cpp")
}

/// A `wide` shim built by [`build_wide_shim`].
struct WideBuild {
    lowered: Lowered,
    out: Output,
    /// The built driver.
    exe: PathBuf,
}

/// Emits the `wide` shim with `opts` (after `tweak` edits the lowering) and builds it with
/// `verilator --build` and the extra Verilator `flags`.
fn build_wide_shim(
    dir: &str,
    opts: &ShimOptions,
    flags: &[&str],
    tweak: fn(&mut Lowered),
) -> WideBuild {
    let case = cases::wide();
    let mut lowered = lower(&case.design, &case.config).unwrap();
    tweak(&mut lowered);
    let dir = cases::scratch(dir);
    let shim = dir.join(format!("{}_shim.cpp", opts.prefix));
    fs::write(&shim, emit_shim(&lowered, opts).unwrap()).unwrap();
    let vlt = dir.join("probes.vlt");
    fs::write(&vlt, emit_vlt(&lowered)).unwrap();
    let cpp = cpp_dir();
    let cflags = format!(
        "-include {0}/simjig_vl_config.h -I{0} -std=gnu++20",
        cpp.display()
    );
    let mdir = dir.join("obj");
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/codegen_it/shim_driver.cpp");
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
        "wide_top",
        "--prefix",
        &opts.prefix,
        "--Mdir",
        mdir.to_str().unwrap(),
        "-CFLAGS",
        &cflags,
    ];
    args.extend_from_slice(flags);
    let out = verilator(&args, &files);
    let exe = mdir.join(&opts.prefix);
    WideBuild { lowered, out, exe }
}

/// Runs the driver built by [`build_wide_shim`] and checks the table it prints.
fn run_wide_driver(build: &WideBuild) {
    assert!(
        build.out.status.success(),
        "model build failed:\n{}",
        stderr(&build.out)
    );
    let run = Command::new(&build.exe)
        .arg(format!("{:x}", build.lowered.ir_hash))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "driver failed:\n{stdout}{}",
        stderr(&run)
    );
    assert_eq!(stdout, expected_table(&build.lowered));
}

fn wide_opts(trace_fst: bool, coverage: bool) -> ShimOptions {
    ShimOptions {
        prefix: "Vwide_top".into(),
        timing: false,
        trace_fst,
        coverage,
    }
}

#[test]
fn shim_builds_against_the_model_and_exports_its_vtable() {
    let lowered = lower(&cases::wide().design, &cases::wide().config).unwrap();
    assert!(
        lowered
            .items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::Struct { .. }) && i.width > 128)
    );
    run_wide_driver(&build_wide_shim(
        "shim_wide",
        &wide_opts(false, false),
        &[],
        |_| {},
    ));
}

/// With trace and coverage on, the shim includes the coverage headers and builds against a
/// `--trace-fst --coverage` model (whose libverilated then has the FST writer and coverage).
#[test]
fn trace_and_coverage_shim_builds_against_a_traced_covered_model() {
    let flags = ["--trace-fst", "--coverage"];
    run_wide_driver(&build_wide_shim(
        "shim_wide_trace_cov",
        &wide_opts(true, true),
        &flags,
        |_| {},
    ));
}

/// A shim that claims `--trace-fst` for a model verilated without it fails to compile with
/// `simjig_model.h`'s message instead of building a model whose waves are silently empty.
#[test]
fn trace_flag_that_disagrees_with_the_model_fails_the_build() {
    let out = build_wide_shim(
        "shim_wide_trace_mismatch",
        &wide_opts(true, false),
        &[],
        |_| {},
    )
    .out;
    assert!(!out.status.success(), "the build should fail");
    assert!(
        stderr(&out).contains("kTraceFst must be true exactly when verilated with --trace-fst"),
        "unexpected failure:\n{}",
        stderr(&out)
    );
}

/// A probe whose width disagrees with the model but fits the same storage class (the 3-bit
/// `state` claimed as 5 bits, both `CData`) builds, and then fails at create.
#[test]
fn probe_with_a_wrong_width_fails_create() {
    let build = build_wide_shim(
        "shim_wide_probe_width",
        &wide_opts(false, false),
        &[],
        |l| {
            let state = l
                .signals
                .iter_mut()
                .find(|s| s.name == "wide_top.state")
                .expect("the wide case probes wide_top.state");
            assert_eq!(state.width, 3);
            state.width = 5;
        },
    );
    assert!(
        build.out.status.success(),
        "model build failed:\n{}",
        stderr(&build.out)
    );
    let run = Command::new(&build.exe)
        .arg(format!("{:x}", build.lowered.ir_hash))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(!run.status.success(), "create should fail:\n{stdout}");
    assert!(
        stdout.contains("vt->create(&opts, &h) == SIMJIG_OK"),
        "failed elsewhere:\n{stdout}"
    );
}
