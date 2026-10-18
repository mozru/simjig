//! The whole pipeline in a simulated build-script environment.

use std::path::{Path, PathBuf};
use std::process::Command;

use simjig_build::svtypes::ir::{Design, Storage};
use simjig_build::svtypes::width::packed_storage;
use simjig_build::{
    Built, CacheStatus, Dut, DutConfig, Emitted, Error, HeaderPort, PortDir, SvModule,
    codegen_emitter,
};

use crate::support::Sandbox;

fn counter(rtl: &Path) -> Dut {
    Dut::new("counter")
        .sources([rtl.join("counter.sv")])
        .include_dirs([rtl])
        .param("WIDTH", 12)
        .top("counter")
}

#[test]
fn builds_counter_end_to_end() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let built = counter(&rtl).build_in(&s.env()).unwrap();

    assert_eq!(built.cache, CacheStatus::Miss);
    assert!(built.prefix.starts_with("Vcounter_"), "{}", built.prefix);
    assert!(built.obj_dir.starts_with(s.path().join("cache/models")));
    assert!(built.header_path().is_file());
    let port = |name: &str, dir, msb| HeaderPort {
        name: name.into(),
        dir,
        msb,
        lsb: 0,
        storage: packed_storage(msb + 1).unwrap(),
    };
    let want = [
        port("clk", PortDir::In, 0),
        port("rst_n", PortDir::In, 0),
        port("count", PortDir::Out, 11),
    ];
    assert_eq!(built.header_ports, want, "WIDTH=12 reached Verilator");
    check_links(&s, &built);
    check_reruns(&rtl, &built.directives);
    // A new Verilator at the same path (an upgrade in place) reruns the build script.
    let exe = format!("cargo::rerun-if-changed={}", s.wrapper().display());
    assert!(built.directives.contains(&exe), "{:?}", built.directives);

    // pixi provides ccache, so the compiler runs through it.
    let wrapper = std::fs::read_to_string(s.path().join("out/simjig-cxx-ccache")).unwrap();
    assert!(wrapper.contains("/ccache' "), "{wrapper}");
}

/// The model, then the runtime it calls, then the C++ stdlib, each pointing at an
/// archive that exists.
fn check_links(s: &Sandbox, built: &Built) {
    let directive = |prefix: &str| -> Vec<String> {
        built
            .directives
            .iter()
            .filter_map(|l| l.strip_prefix(prefix).map(str::to_owned))
            .collect()
    };
    let links = directive("cargo::rustc-link-lib=");
    let searches = directive("cargo::rustc-link-search=native=");
    assert!(
        links[0].starts_with(&format!("static=simjig_model_{}_", built.prefix)),
        "{links:?}"
    );
    assert!(
        links[1].starts_with("static=simjig_verilated_"),
        "{links:?}"
    );
    assert!(matches!(links[2].as_str(), "c++" | "stdc++"), "{links:?}");
    for (dir, link) in searches.iter().zip(&links) {
        let name = link.trim_start_matches("static=");
        assert!(
            Path::new(dir).join(format!("lib{name}.a")).is_file(),
            "{dir} {link}"
        );
    }
    assert!(Path::new(&searches[0]).starts_with(s.path().join("cache/archives")));
    assert!(Path::new(&searches[1]).starts_with(s.path().join("cache/libverilated")));
}

/// Exactly the files Verilator read, plus the tracked variables.
fn check_reruns(rtl: &Path, directives: &[String]) {
    let reruns: Vec<&str> = directives
        .iter()
        .filter_map(|l| l.strip_prefix("cargo::rerun-if-changed="))
        .collect();
    let rtl_reruns: Vec<&str> = reruns
        .iter()
        .copied()
        .filter(|r| Path::new(r).starts_with(rtl))
        .collect();
    let want = [rtl.join("counter.sv"), rtl.join("defs.svh")];
    assert_eq!(
        rtl_reruns,
        want.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>()
    );
    assert!(
        reruns.iter().any(|r| r.ends_with("verilated_std.sv")),
        "{reruns:?}"
    );
    for var in [
        "SIMJIG_CXX_OPT",
        "VERILATOR_ROOT",
        "CXX",
        "SIMJIG_VERILATOR",
        "SIMJIG_CACHE_DIR",
    ] {
        let line = format!("cargo::rerun-if-env-changed={var}");
        assert!(directives.contains(&line), "{var}");
    }
    // The resolved Verilator is tracked instead (see the caller).
    assert!(!directives.contains(&"cargo::rerun-if-env-changed=PATH".to_owned()));
}

/// An emitter that claims `count` is 8 bits wide.
#[expect(clippy::unnecessary_wraps, reason = "the `Emitter` signature")]
fn wrong_ports(_: &Design, _: &DutConfig) -> Result<Emitted, Error> {
    Ok(Emitted {
        ports: Some(vec![HeaderPort {
            name: "count".into(),
            dir: PortDir::Out,
            msb: 7,
            lsb: 0,
            storage: Storage::CData,
        }]),
        ..Emitted::default()
    })
}

#[test]
fn expected_ports_are_checked_against_the_header() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let err = counter(&rtl)
        .emitter(wrong_ports)
        .build_in(&s.env())
        .unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, Error::Ports(_)), "{msg}");
    assert!(
        msg.contains(
            "`count`: expected Out count[7:0] as CData, header declares Out count[11:0] as SData"
        ),
        "{msg}"
    );
    assert!(
        msg.contains("not expected: header declares In clk[0:0] as CData"),
        "{msg}"
    );
}

/// The JSON pass runs the Verilator the build located (`SIMJIG_VERILATOR` here), not
/// whatever `verilator` is first on `PATH`: the IR and the model must come from one tool.
#[test]
fn the_json_pass_runs_the_located_verilator() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let mut env = s.env();
    env.path = Some(s.fake_verilator_first_on_path());
    counter(&rtl).build_in(&env).unwrap();
    assert!(
        s.verilator_calls().contains("--json-only"),
        "{}",
        s.verilator_calls()
    );
}

/// Likewise with `VERILATOR_ROOT`: `$VERILATOR_ROOT/bin/verilator` runs every pass.
#[test]
fn the_json_pass_follows_verilator_root() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let root = Command::new(crate::support::real_verilator())
        .args(["--getenv", "VERILATOR_ROOT"])
        .output()
        .unwrap();
    let root = PathBuf::from(String::from_utf8(root.stdout).unwrap().trim());
    let mut env = s.env();
    env.path = Some(s.fake_verilator_first_on_path());
    env.simjig_verilator = None;
    env.verilator_root = Some(root.clone());
    let built = counter(&rtl).build_in(&env).unwrap();
    assert_eq!(built.verilator.exe, root.join("bin/verilator"));
}

/// A define value containing `+` (which `+define+` would split) means the same to the
/// JSON pass and the model: one argument builder spells both command lines.
#[test]
fn defines_reach_both_passes_alike() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let built = Dut::new("defines")
        .sources([rtl.join("defines.sv")])
        .top("defines")
        .define("DEPTH", "4+4")
        .build_in(&s.env())
        .unwrap();
    assert_eq!(built.header_ports[0].msb, 7, "the model saw DEPTH = 8");
    let rust = std::fs::read_to_string(built.rust_file.unwrap()).unwrap();
    assert!(
        rust.contains("pub const D: ::core::primitive::i32 = 8;"),
        "{rust}"
    );
}

/// A `.timing(true)` DUT is elaborated with `--timing` by the JSON pass too: it accepts
/// `fork`/`#`/`@(e)`, and sees the same `VERILATOR_TIMING` branch as the model.
#[test]
fn timing_reaches_the_json_pass() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let built = Dut::new("timing")
        .sources([rtl.join("timing.sv")])
        .top("timing")
        .timing(true)
        .build_in(&s.env())
        .unwrap();
    let q = &built.design.top_module().ports[1];
    assert_eq!(built.design.ty(q.ty).width, Some(8));
    assert_eq!(
        built
            .header_ports
            .iter()
            .find(|p| p.name == "q")
            .unwrap()
            .msb,
        7
    );
}

/// A layout check that always fails, as a mismatch would.
#[expect(clippy::unnecessary_wraps, reason = "the `Emitter` signature")]
fn failing_layout_check(_: &Design, _: &DutConfig) -> Result<Emitted, Error> {
    Ok(Emitted {
        layout_check: Some(SvModule {
            module: "always_mismatched".into(),
            contents: "module always_mismatched;\n  if (1) begin : g\n    \
                       $fatal(1, \"layout mismatch\");\n  end\nendmodule\n"
                .into(),
        }),
        ..Emitted::default()
    })
}

/// The layout-check flags come after the user's Verilator arguments, so a user's
/// `-Wno-USERFATAL` cannot turn a layout mismatch into a warning.
#[test]
fn user_args_cannot_disable_the_layout_check() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let err = counter(&rtl)
        .emitter(failing_layout_check)
        .verilator_args(["-Wno-USERFATAL"])
        .build_in(&s.env())
        .unwrap_err();
    let Error::Command {
        command, stderr, ..
    } = &err
    else {
        panic!("expected the layout check to fail, got {err}");
    };
    assert!(
        command.ends_with("--lint-only -Werror-USERFATAL"),
        "{command}"
    );
    assert!(stderr.contains("layout mismatch"), "{stderr}");
}

/// DPI exports are global C symbols: a second DUT exporting the same name would bind to
/// whichever archive the linker saw first, so the build script fails instead.
#[test]
fn duplicate_dpi_exports_across_duts_fail() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let dut = |name: &str| {
        Dut::new(name)
            .sources([rtl.join("dpi_export.sv")])
            .top("dpi_export")
    };
    dut("first").build_in(&s.env()).unwrap();
    // The same configuration again is the same model, not a second export.
    dut("first").build_in(&s.env()).unwrap();
    let err = dut("second").build_in(&s.env()).unwrap_err().to_string();
    assert!(
        err.contains("DPI export `dpi_export_get`") && err.contains("DUT `first`"),
        "{err}"
    );
}

/// Two covergroup models of one build script must agree on `--coverage`: Verilator
/// compiles its covergroup runtime per model from `VM_COVERAGE`, and only one copy links.
#[test]
fn covergroup_models_with_mixed_coverage_fail() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let dut = |name: &str| {
        Dut::new(name)
            .sources([rtl.join("cg_user.sv")])
            .top("cg_user")
    };
    dut("plain").build_in(&s.env()).unwrap();
    dut("plain_again").build_in(&s.env()).unwrap();
    let err = dut("covered")
        .coverage(true)
        .build_in(&s.env())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("uses covergroups") && err.contains("DUT `plain"),
        "{err}"
    );
}

/// A real layout mismatch (the code generator run on an IR whose struct is one bit
/// wider than Verilator lays it out) fails the build even under a user's
/// `-Wno-USERFATAL`, with the check's own message.
#[test]
fn a_user_waiver_cannot_hide_a_real_layout_mismatch() {
    fn widened(design: &Design, config: &DutConfig) -> Result<Emitted, Error> {
        let mut wrong = design.clone();
        let ty = wrong
            .types
            .iter_mut()
            .find(|t| t.name.as_deref() == Some("struct_pipe_pkg::beat_t"))
            .expect("struct_pipe_pkg::beat_t in the IR");
        ty.width = ty.width.map(|w| w + 1);
        codegen_emitter(&wrong, config)
    }
    let s = Sandbox::new();
    let d = crate::support::dut("struct_pipe");
    let err = Dut::new("struct_pipe")
        .sources([d.join("struct_pipe_pkg.sv"), d.join("struct_pipe.sv")])
        .top("struct_pipe")
        .verilator_args(["-Wno-USERFATAL"])
        .emitter(widened)
        .build_in(&s.env())
        .unwrap_err();
    let Error::Command { stderr, .. } = &err else {
        panic!("expected the layout check to fail, got {err}");
    };
    assert!(
        stderr.contains("%Error-USERFATAL") && stderr.contains("differs from the IR width"),
        "{stderr}"
    );
    assert_eq!(s.verilations(), 0);
}

/// Coverage switched on where simjig cannot see it (an option file) builds a model whose
/// `VM_COVERAGE` disagrees with the shim; the build stops and says so.
#[test]
fn an_option_file_that_changes_coverage_fails_the_build() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let opts = s.path().join("cov.f");
    std::fs::write(&opts, "--coverage-line\n").unwrap();
    let err = counter(&rtl)
        .verilator_args(["-f".to_owned(), opts.display().to_string()])
        .build_in(&s.env())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("VM_COVERAGE = 1") && err.contains("option file"),
        "{err}"
    );
}

/// A generated `.sv` input is part of the design in every Verilator run: the JSON pass and
/// the layout check see it as well as the model build, so a package it declares can be
/// imported by the user's sources.
#[test]
fn generated_sv_inputs_reach_every_verilator_run() {
    let s = Sandbox::new();
    let top = s.path().join("uses_pkg.sv");
    std::fs::write(
        &top,
        "module uses_pkg import gen_pkg::*; (input logic [W-1:0] a, output word_t y);\n  \
         assign y = word_t'(a);\nendmodule\n",
    )
    .unwrap();
    let built = Dut::new("uses_pkg")
        .sources([&top])
        .top("uses_pkg")
        .verilator_input(
            "gen_pkg.sv",
            "package gen_pkg;\n  localparam int W = 6;\n  typedef logic [W-1:0] word_t;\nendpackage\n",
        )
        .build_in(&s.env())
        .unwrap();
    let a = &built.design.top_module().ports[0];
    assert_eq!(built.design.ty(a.ty).width, Some(6));
    let calls = s.verilator_calls();
    let runs = calls.lines().filter(|l| l.contains("gen_pkg.sv")).count();
    // A DUT without interface ports: one JSON pass, the layout check, the model build.
    assert_eq!(runs, 3, "{calls}");
}

/// The generated module of the DUT `name` in `s`'s `OUT_DIR`.
fn generated(s: &Sandbox, name: &str) -> String {
    std::fs::read_to_string(s.path().join(format!("out/simjig/{name}.rs"))).unwrap()
}

/// A built DUT sets its gate; a failed one replaces the module an earlier build left
/// (here copied in, as a new build-script process finds it) with a `compile_error!` stub
/// naming the DUT, so an `include_dut!` of it cannot compile against a model that is not
/// linked.
#[test]
fn a_failed_build_stubs_the_module_an_earlier_build_left() {
    let good = Sandbox::new();
    let built = counter(&good.rtl()).build_in(&good.env()).unwrap();
    for line in [
        "cargo::rustc-check-cfg=cfg(simjig_dut_counter)",
        "cargo::rustc-cfg=simjig_dut_counter",
    ] {
        assert!(built.directives.iter().any(|d| d == line), "{line}");
    }
    let s = Sandbox::new();
    std::fs::create_dir_all(s.path().join("out/simjig")).unwrap();
    std::fs::write(
        s.path().join("out/simjig/counter.rs"),
        generated(&good, "counter"),
    )
    .unwrap();
    let missing = s.path().join("missing.sv");
    let err = Dut::new("counter")
        .sources([&missing])
        .top("counter")
        .build_in(&s.env())
        .unwrap_err();
    let stub = generated(&s, "counter");
    assert!(
        stub.contains("::core::compile_error!(\"DUT `counter` failed to build"),
        "{stub}"
    );
    assert!(
        stub.contains("missing.sv"),
        "the stub carries the error: {stub}\n{err}"
    );
    assert!(!stub.contains("simjig_vtable"), "{stub}");
}

/// The module this build wrote before a later step failed (here the layout check) is
/// stubbed too.
#[test]
fn a_build_failing_after_codegen_stubs_its_own_module() {
    fn codegen_then_fail(design: &Design, config: &DutConfig) -> Result<Emitted, Error> {
        Ok(Emitted {
            layout_check: failing_layout_check(design, config)?.layout_check,
            ..codegen_emitter(design, config)?
        })
    }
    let s = Sandbox::new();
    counter(&s.rtl())
        .emitter(codegen_then_fail)
        .build_in(&s.env())
        .unwrap_err();
    let stub = generated(&s, "counter");
    assert!(
        stub.contains("compile_error!(\"DUT `counter` failed"),
        "{stub}"
    );
}

/// A failed DUT whose name an earlier DUT of the same build script built keeps that
/// DUT's module: the file is the earlier one's.
#[test]
fn a_failed_duplicate_name_keeps_the_built_module() {
    let s = Sandbox::new();
    counter(&s.rtl()).build_in(&s.env()).unwrap();
    let before = generated(&s, "counter");
    Dut::new("counter")
        .top("counter")
        .build_in(&s.env())
        .unwrap_err();
    assert_eq!(generated(&s, "counter"), before);
}
