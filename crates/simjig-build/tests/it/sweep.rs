//! `Sweep`: several configurations of one module built as one Rust module, and the
//! refusals that keep its files apart from other DUTs'.

use std::path::Path;

use simjig_build::{Dut, Error, Sweep, noop_emitter};

use crate::support::Sandbox;

fn counter(rtl: &Path, name: &str, width: u32) -> Dut {
    Dut::new(name)
        .sources([rtl.join("counter.sv")])
        .include_dirs([rtl])
        .param("WIDTH", width)
        .top("counter")
}

fn generated(s: &Sandbox, name: &str) -> String {
    std::fs::read_to_string(s.path().join(format!("out/simjig/{name}.rs"))).unwrap_or_default()
}

/// A message of a refused sweep (the error's `Display`).
fn refused(result: Result<simjig_build::BuiltSweep, Error>) -> String {
    match result {
        Ok(built) => panic!("built: {}", built.rust_file.display()),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_sweep_writes_one_module_over_its_members() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let built = Sweep::new("counters")
        .dut(counter(&rtl, "ctr8", 8))
        .dut(counter(&rtl, "ctr12", 12))
        .build_in(&s.env())
        .unwrap();
    assert_eq!(built.rust_file, s.path().join("out/simjig/counters.rs"));
    assert_eq!(built.duts.len(), 2);
    assert!(
        built
            .directives
            .contains(&"cargo::rustc-cfg=simjig_dut_counters".to_owned()),
        "{:?}",
        built.directives
    );
    let sweep = generated(&s, "counters");
    for want in [
        "pub trait Ports:",
        "type Count: ::simjig::__private::Vector;",
        "pub mod ctr8 {\n    ::core::include!(\"ctr8.rs\");\n}",
        "impl Ports for ctr12::Dut {",
        "type Count = ::simjig::__private::bits::Bits<12>;",
        "/// Configuration `ctr12` of `counter`: `WIDTH` = 12.",
    ] {
        assert!(sweep.contains(want), "no {want:?} in\n{sweep}");
    }
    // Each member's module is rewritten for the sweep, and still links its own model.
    let member = generated(&s, "ctr8");
    assert!(
        member.contains("A configuration of sweep `counters`"),
        "{member}"
    );
    assert!(
        member.contains(&format!("prefix = {:?}", built.duts[0].prefix)),
        "{member}"
    );
}

#[test]
fn members_of_two_modules_are_refused_and_leave_a_stub() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let ports = Dut::new("ports_dut")
        .sources([rtl.join("ports.sv")])
        .top("ports");
    let msg = refused(
        Sweep::new("mixed")
            .dut(counter(&rtl, "ctr", 8))
            .dut(ports)
            .build_in(&s.env()),
    );
    assert!(msg.contains("generating code for DUT `mixed`"), "{msg}");
    let stub = generated(&s, "mixed");
    assert!(stub.contains("compile_error!"), "{stub}");
    assert!(stub.contains("sweep `mixed` failed to build"), "{stub}");
}

#[test]
fn a_failed_member_fails_the_sweep() {
    let s = Sandbox::new();
    let msg = refused(Sweep::new("one").dut(Dut::new("empty")).build_in(&s.env()));
    assert!(msg.contains("invalid DUT `empty` configuration"), "{msg}");
    assert!(generated(&s, "one").contains("compile_error!"));
}

#[test]
fn a_sweep_cannot_take_a_dut_name_and_a_dut_cannot_take_a_sweep_name() {
    let s = Sandbox::new();
    let env = s.env();
    // A DUT that fails still claims its name (and file).
    assert!(Dut::new("taken").build_in(&env).is_err());
    let msg = refused(Sweep::new("taken").dut(Dut::new("x")).build_in(&env));
    assert!(
        msg.contains("a DUT of this build script is named `taken`"),
        "{msg}"
    );

    assert!(
        Sweep::new("sweep_a")
            .dut(Dut::new("m"))
            .build_in(&env)
            .is_err()
    );
    let msg = Dut::new("Sweep_A").build_in(&env).unwrap_err().to_string();
    assert!(msg.contains("has a sweep named `sweep_a`"), "{msg}");
    let msg = refused(Sweep::new("sweep_b").dut(Dut::new("m")).build_in(&env));
    assert!(
        msg.contains("DUT `m` is already in sweep `sweep_a`"),
        "{msg}"
    );
}

#[test]
fn a_member_with_its_own_emitter_is_refused() {
    let s = Sandbox::new();
    let custom = Dut::new("c").emitter(noop_emitter);
    let msg = refused(Sweep::new("sw").dut(custom).build_in(&s.env()));
    assert!(msg.contains("DUT `c` has a custom emitter"), "{msg}");
    let msg = refused(Sweep::new("none").build_in(&s.env()));
    assert!(msg.contains("at least one DUT"), "{msg}");
    let msg = refused(
        Sweep::new("not-an-ident")
            .dut(Dut::new("d"))
            .build_in(&s.env()),
    );
    assert!(msg.contains("C identifier"), "{msg}");
}
