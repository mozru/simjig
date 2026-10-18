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
            "elaborating DUT `broken` failed (exit status: 1) at {}:",
            broken.display()
        )),
        "{msg}"
    );
    assert!(
        msg.ends_with(&format!(
            "\ncommand: {command}\n(its `-Mdir` was a temporary directory, removed after the \
             run)"
        )),
        "{msg}"
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
    let (mut outs, mut assigns) = (String::new(), String::new());
    for i in 0..12 {
        let o = format!("o{i}");
        outs.push_str(", output logic ");
        outs.push_str(&o);
        assigns.push_str("  assign ");
        assigns.push_str(&o);
        assigns.push_str(" = in.v;\n");
    }
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
        first.contains(&format!("at {}:", top.display())) && first.contains("nosuch_sig"),
        "{first}\n{err}"
    );
    assert!(!err.contains("PINMISSING"), "{err}");
    assert!(!err.contains("hint: Verilator's lint warnings"), "{err}");
}

/// A `rate_limiter` with two parameters, and the same module behind an interface port.
fn rate_limiter(s: &Sandbox, iface: bool) -> Dut {
    let rtl = s.path().join("rl");
    std::fs::create_dir_all(&rtl).unwrap();
    let ports = if iface {
        "tick_if.sink t, output logic y"
    } else {
        "input logic clk, output logic y"
    };
    let body = if iface { "t.v" } else { "clk" };
    std::fs::write(
        rtl.join("rl.sv"),
        format!(
            "interface tick_if; logic v; modport sink(input v); endinterface\n\
             module rate_limiter #(parameter int BURST = 2, parameter int PERIOD = 4) \
             ({ports});\n  assign y = {body};\nendmodule\n"
        ),
    )
    .unwrap();
    let d = Dut::new("rl_small")
        .sources([rtl.join("rl.sv")])
        .top("rate_limiter")
        .param("PERIDO", 3);
    if iface {
        d.bind_iface("t", "tick_if.sink")
    } else {
        d
    }
}

/// A misspelled `.param` names the module's parameters, not Verilator's message about the
/// command line (`-G`) or the generated wrapper (`PINNOTFOUND`, located in the cache).
#[test]
fn a_misspelled_param_names_the_modules_parameters() {
    for iface in [false, true] {
        let s = Sandbox::new();
        let err = rate_limiter(&s, iface).build_in(&s.env()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid DUT `rl_small` configuration: module `rate_limiter` has no parameter \
             `PERIDO`; did you mean `PERIOD`? (it has BURST, PERIOD)",
            "interface port: {iface}"
        );
    }
}

/// An accumulator whose interface port has its own width `W`, like the module's, and a
/// localparam `L`; `iface` puts the input behind the port.
fn acc(s: &Sandbox, name: &str, iface: bool) -> Dut {
    let rtl = s.path().join("acc");
    std::fs::create_dir_all(&rtl).unwrap();
    let (ports, rhs) = if iface {
        ("acc_if.sink up", "up.d")
    } else {
        ("input logic [W-1:0] d", "d")
    };
    std::fs::write(
        rtl.join("acc.sv"),
        format!(
            "interface acc_if #(parameter int W = 8);\n  localparam int K = 1;\n  \
             logic [W-1:0] d;\n  modport sink(input d);\nendinterface\n\
             module acc #(parameter int W = 8)({ports}, output logic [W-1:0] q);\n  \
             localparam int L = 3;\n  assign q = {rhs};\nendmodule\n"
        ),
    )
    .unwrap();
    Dut::new(name).sources([rtl.join("acc.sv")]).top("acc")
}

/// A misspelled interface parameter names the interface's parameters, like a misspelled
/// `.param` names the module's, instead of a `PINNOTFOUND` in the generated top.
#[test]
fn a_misspelled_interface_param_names_the_interfaces_parameters() {
    let s = Sandbox::new();
    let err = acc(&s, "acc_ww", true)
        .param("W", 12)
        .bind_iface_with_params("up", "acc_if.sink", [("WW", 12)])
        .build_in(&s.env())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid DUT `acc_ww` configuration: interface `acc_if` (bound to `up`) has no \
         parameter `WW`; did you mean `W`? (it has W)"
    );
}

/// Overriding a localparam says so, for the module (by `-G` or through the generated
/// top) and for an interface.
#[test]
fn overriding_a_localparam_says_it_is_one() {
    for iface in [false, true] {
        let s = Sandbox::new();
        let d = acc(&s, "acc_l", iface).param("L", 1);
        let d = if iface {
            d.bind_iface("up", "acc_if.sink")
        } else {
            d
        };
        assert_eq!(
            d.build_in(&s.env()).unwrap_err().to_string(),
            "invalid DUT `acc_l` configuration: module `acc` has no parameter `L`: `L` is a \
             localparam and cannot be overridden (it has W)",
            "interface port: {iface}"
        );
    }
    let s = Sandbox::new();
    let err = acc(&s, "acc_k", true)
        .bind_iface_with_params("up", "acc_if.sink", [("K", 2)])
        .build_in(&s.env())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid DUT `acc_k` configuration: interface `acc_if` (bound to `up`) has no \
         parameter `K`: `K` is a localparam and cannot be overridden (it has W)"
    );
}

/// The widths of the built top's ports, by name.
fn port_widths(b: &simjig_build::Built) -> Vec<(String, Option<u32>)> {
    let d = &b.design;
    d.top_module()
        .ports
        .iter()
        .map(|p| (p.name.clone(), d.types[p.ty.0 as usize].width))
        .collect()
}

/// A `.param` reaches a bound interface's same-named parameter when the binding leaves
/// it unset (under `-Wno-fatal` the 8-bit interface fed the 12-bit `q` silently before
/// this default; checked against 5.052). A value the binding sets wins, even when it
/// differs from the `.param`: the shared name is only a heuristic.
#[test]
fn an_interface_parameter_defaults_to_the_modules() {
    let s = Sandbox::new();
    let built = acc(&s, "acc_tied", true)
        .param("W", 12)
        .bind_iface("up", "acc_if.sink")
        .verilator_args(["-Wno-fatal"])
        .build_in(&s.env())
        .unwrap();
    assert_eq!(
        port_widths(&built),
        [("q".to_owned(), Some(12)), ("up_d".to_owned(), Some(12))]
    );
    let s = Sandbox::new();
    let apart = acc(&s, "acc_apart", true)
        .param("W", 12)
        .bind_iface_with_params("up", "acc_if.sink", [("W", 8)])
        .verilator_args(["-Wno-fatal"])
        .build_in(&s.env())
        .unwrap();
    assert_eq!(
        port_widths(&apart),
        [("q".to_owned(), Some(12)), ("up_d".to_owned(), Some(8))]
    );
}

/// A module whose output interface is legitimately wider than its `W` (it reads the
/// width through the port, `out.W`). IEEE 1800-2017 §25.5 interface port headers
/// carry no parameter assignments, so nothing in the module ties `out`'s `W` to the
/// module's: the explicit binding value is the user's only say, and must be honoured.
/// It was refused while a binding value had to equal the same-named `.param`, though it
/// built with `.param("W", 8)` (the default) left out.
#[test]
fn an_explicit_interface_param_is_not_refused_by_name() {
    let s = Sandbox::new();
    let rtl = s.path().join("widen");
    std::fs::create_dir_all(&rtl).unwrap();
    std::fs::write(
        rtl.join("widen.sv"),
        "interface s_if #(parameter int W = 8);\n  logic [W-1:0] d;\n  \
         modport sink(input d);\n  modport source(output d);\nendinterface\n\
         module widen #(parameter int W = 8)(s_if.sink in, s_if.source out);\n  \
         assign out.d = {in.d, in.d};\nendmodule\n",
    )
    .unwrap();
    let dut = |name: &str, param: bool| {
        let d = Dut::new(name).sources([rtl.join("widen.sv")]).top("widen");
        let d = if param { d.param("W", 8) } else { d };
        d.bind_iface("in", "s_if.sink")
            .bind_iface_with_params("out", "s_if.source", [("W", 16)])
    };
    let default = dut("widen_default", false).build_in(&s.env());
    assert!(default.is_ok(), "{:?}", default.err());
    let explicit = dut("widen_explicit", true).build_in(&s.env());
    assert!(
        explicit.is_ok(),
        "`.param(\"W\", 8)`, the default, must not refuse what builds without it: {}",
        explicit.unwrap_err()
    );
}

/// The `simjig:` warnings among a build's directives.
fn simjig_warnings(b: &simjig_build::Built) -> Vec<&str> {
    b.directives
        .iter()
        .filter_map(|d| d.strip_prefix("cargo::warning=simjig: "))
        .collect()
}

/// A module whose `WIDTH` defaults to 16, bound to an interface whose `WIDTH` defaults
/// to 8: the shape that builds silently with the wrong widths under `-Wno-fatal`.
fn up16(s: &Sandbox, name: &str) -> Dut {
    let rtl = s.path().join("up16");
    std::fs::create_dir_all(&rtl).unwrap();
    std::fs::write(
        rtl.join("up16.sv"),
        "interface bus_if #(parameter int WIDTH = 8);\n  logic [WIDTH-1:0] d;\n  \
         modport sink(input d);\nendinterface\n\
         module up16 #(parameter int WIDTH = 16)(bus_if.sink up, \
         output logic [WIDTH-1:0] q);\n  assign q = up.d;\nendmodule\n",
    )
    .unwrap();
    Dut::new(name)
        .sources([rtl.join("up16.sv")])
        .top("up16")
        .verilator_args(["-Wno-fatal"])
}

/// When neither side sets a parameter that both declare and their values differ, the
/// build warns, naming both values and the call that settles it; a cached rerun warns
/// again (the warning is read from the IR, which the front-end cache keeps).
#[test]
fn differing_interface_parameter_defaults_warn() {
    let s = Sandbox::new();
    let expected = "DUT `up16_unset`: module `up16` has `WIDTH` = 16 but interface `bus_if` \
         (bound to `up`) has `WIDTH` = 8, and neither is set; simjig ties a `.param` to a \
         same-named interface parameter only by name, a heuristic (interface ports carry \
         no parameters, IEEE 1800-2017 §25.5). If they are one quantity, set it with \
         `.bind_iface_with_params(\"up\", \"bus_if.sink\", [(\"WIDTH\", 16)])` or \
         `.param(\"WIDTH\", 16)`, which reaches both; if the interface's 8 is intended, \
         bind it: `.bind_iface_with_params(\"up\", \"bus_if.sink\", [(\"WIDTH\", 8)])` \
         changes no width; setting either explicitly silences this";
    for _ in 0..2 {
        let built = up16(&s, "up16_unset")
            .bind_iface("up", "bus_if.sink")
            .build_in(&s.env())
            .unwrap();
        assert_eq!(simjig_warnings(&built), [expected]);
    }
    // Equal defaults are not a mismatch.
    let same = acc(&s, "acc_defaults", true)
        .bind_iface("up", "acc_if.sink")
        .build_in(&s.env())
        .unwrap();
    assert_eq!(simjig_warnings(&same), Vec::<&str>::new());
}

/// An explicit value on either side silences the warning: a `.param` (which the
/// binding then takes), or a binding value, even one that leaves the two apart.
#[test]
fn an_explicit_interface_parameter_silences_the_warning() {
    let s = Sandbox::new();
    let cases = [
        (
            "up16_param",
            up16(&s, "up16_param")
                .param("WIDTH", 16)
                .bind_iface("up", "bus_if.sink"),
        ),
        (
            "up16_bound",
            up16(&s, "up16_bound").bind_iface_with_params("up", "bus_if.sink", [("WIDTH", 16)]),
        ),
        (
            "up16_apart",
            up16(&s, "up16_apart").bind_iface_with_params("up", "bus_if.sink", [("WIDTH", 8)]),
        ),
    ];
    for (name, dut) in cases {
        let built = dut.build_in(&s.env()).unwrap();
        assert_eq!(simjig_warnings(&built), Vec::<&str>::new(), "{name}");
    }
}

/// A gearbox whose `W_OUT` defaults to 16, bound to an interface whose `W_OUT` defaults
/// to `iface_w_out`, under Verilator's default lint (fatal warnings). It drives `out.d`
/// with `reps` copies of its 4-bit input.
fn gearbox(s: &Sandbox, name: &str, iface_w_out: u32, reps: &str) -> Dut {
    let rtl = s.path().join(name);
    std::fs::create_dir_all(&rtl).unwrap();
    std::fs::write(
        rtl.join("gearbox.sv"),
        format!(
            "interface out_if #(parameter int W_OUT = {iface_w_out});\n  logic [W_OUT-1:0] d;\n  \
             modport src(output d);\nendinterface\n\
             module gearbox #(parameter int W_IN = 4, parameter int W_OUT = 16)\
             (input logic [W_IN-1:0] x, out_if.src out);\n  \
             assign out.d = {{({reps}){{x}}}};\nendmodule\n"
        ),
    )
    .unwrap();
    Dut::new(name)
        .sources([rtl.join("gearbox.sv")])
        .top("gearbox")
        .bind_iface("out", "out_if.src")
}

const GEARBOX_HINT: &str = "hint: the width error may come from interface `out_if` (bound \
     to `out`) defaulting `W_OUT` to 8 while `gearbox` defaults it to 16; if they are one \
     quantity, set it with `.bind_iface_with_params(\"out\", \"out_if.src\", \
     [(\"W_OUT\", 16)])` or `.param(\"W_OUT\", 16)`, which reaches both; if the \
     interface's 8 is intended, bind it: `.bind_iface_with_params(\"out\", \"out_if.src\", \
     [(\"W_OUT\", 8)])` changes no width\n";

/// Under the default fatal lint, a default mismatch stops the model build at a width
/// warning inside the generated wrapper, before the mismatch warning could be printed: the
/// error leads with a hint naming the two defaults. So does a JSON pass that a
/// `-Werror-WIDTHTRUNC` stops, whose IR comes from a diagnosis pass.
#[test]
fn an_interface_default_mismatch_leads_a_width_error() {
    let s = Sandbox::new();
    let cases = [
        (
            "gb_lint",
            gearbox(&s, "gb_lint", 8, "W_OUT/W_IN"),
            "verilating DUT `gb_lint` failed",
        ),
        (
            "gb_werror",
            gearbox(&s, "gb_werror", 8, "W_OUT/W_IN").verilator_args(["-Werror-WIDTHTRUNC"]),
            "elaborating DUT `gb_werror` failed",
        ),
    ];
    for (name, dut, headline) in cases {
        let msg = dut.build_in(&s.env()).unwrap_err().to_string();
        let rest = msg
            .strip_prefix(GEARBOX_HINT)
            .unwrap_or_else(|| panic!("{name}: {msg}"));
        assert!(rest.starts_with(headline), "{name}: {msg}");
        assert!(rest.contains("WIDTHTRUNC"), "{name}: {msg}");
    }
}

/// A width error with equal defaults (a bug in the RTL) gets no interface hint.
#[test]
fn equal_interface_defaults_leave_a_width_error_unhinted() {
    let s = Sandbox::new();
    let msg = gearbox(&s, "gb_same", 16, "W_OUT/W_IN+1")
        .build_in(&s.env())
        .unwrap_err()
        .to_string();
    assert!(msg.starts_with("verilating DUT `gb_same` failed"), "{msg}");
    assert!(
        msg.contains("WIDTHTRUNC") && !msg.contains("may come from"),
        "{msg}"
    );
}
