//! Parameter sweeps: `lower_sweep`'s decisions on `cases/sweep` (two parameter sets of
//! `sw_top`) and `cases/iface_param` (an interface port whose bundle differs), snapshots
//! of the sweep module and its members, and the committed generated files compiled with a
//! test body written once over the sweep's `Ports` trait.

use std::path::Path;

use simjig_bits::{Bit, Bits, WideBits};
use simjig_build::codegen::{
    AssocBound, Error, Lowered, LoweredSweep, MethodKind, RustOptions, RustType, TraitType,
    emit_sweep, emit_sweep_member, lower, lower_sweep,
};

use crate::__private::{In, Out, RecordingBinder, Value, Vector};
use crate::cases::{self, Case};

fn lowered(cases: &[Case]) -> Vec<(String, Lowered)> {
    cases
        .iter()
        .map(|c| (c.name.clone(), lower(&c.design, &c.config).unwrap()))
        .collect()
}

fn member_options(case: &Case) -> RustOptions {
    RustOptions {
        prefix: format!("V{}", case.name),
        precision_exponent: case.design.time_precision.0,
        dpi_imports: Vec::new(),
    }
}

/// The sweep module and each member's file, by file name.
fn files(name: &str, cases: &[Case]) -> (LoweredSweep, Vec<(String, String)>) {
    let members = lowered(cases);
    let sweep = lower_sweep(name, &members).unwrap();
    let mut out = vec![(format!("{name}.rs"), emit_sweep(&sweep))];
    for (case, (member, l)) in cases.iter().zip(&members) {
        let text = emit_sweep_member(l, &member_options(case), &sweep);
        out.push((format!("{member}.rs"), text));
    }
    (sweep, out)
}

/// Snapshots each file, and checks it against the copy under `generated/<dir>/` that
/// the compile tests below include (`SIMJIG_BLESS=1` rewrites the copies).
fn check_generated(dir: &str, files: &[(String, String)]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/codegen_it/generated")
        .join(dir);
    let bless = std::env::var_os("SIMJIG_BLESS").is_some();
    for (file, text) in files {
        insta::assert_snapshot!(format!("{dir}__{file}"), text);
        let path = root.join(file);
        if bless {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == *text,
            "{} is stale; rerun with SIMJIG_BLESS=1",
            path.display()
        );
    }
}

#[test]
fn a_sweep_shares_what_is_alike_and_abstracts_the_rest() {
    let (sweep, files) = files("sw", &cases::sweep_members());
    check_generated("sweep", &files);
    // The package enum and struct are alike: defined once, imported by each member.
    let shared: Vec<&str> = sweep.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(shared, ["State", "Hdr"], "{:?}", sweep.reexports);
    assert!(sweep.reexports.iter().any(|r| r == "sw_pkg"));
    let t = &sweep.ports;
    let ty = |method: &str| {
        let m = t.methods.iter().find(|m| m.name == method).unwrap();
        match &m.kind {
            MethodKind::Signal { ty, .. } | MethodKind::Bundle(ty) => ty.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(ty("clk"), TraitType::Concrete(RustType::Bit));
    assert_eq!(
        ty("hdr_in"),
        TraitType::Concrete(RustType::Named("Hdr".into()))
    );
    // `a` and `b` agree in every member, so they share one associated type.
    assert_eq!(ty("a"), ty("b"));
    let names: Vec<(&str, &AssocBound, Vec<&str>)> = t
        .types
        .iter()
        .map(|a| {
            let users = a.users.iter().map(String::as_str).collect();
            (a.name.as_str(), &a.bound, users)
        })
        .collect();
    assert_eq!(
        names,
        [
            ("A", &AssocBound::Vector, vec!["a", "b"]),
            ("Sum", &AssocBound::Vector, vec!["sum"]),
            ("Word", &AssocBound::Value, vec!["word"]),
            ("Lanes", &AssocBound::None, vec!["lanes"]),
        ]
    );
    let consts: Vec<&str> = t.consts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(consts, ["W", "N"]);
    assert_eq!(t.omitted, Vec::<String>::new());
}

#[test]
fn an_interface_bundle_that_differs_gets_a_trait() {
    let cases = [
        cases::iface_sweep_member("r_k3", 3, None),
        cases::iface_sweep_member("r_k5", 5, Some(8)),
    ];
    let (sweep, files) = files("r_sweep", &cases);
    check_generated("iface_sweep", &files);
    assert_eq!(sweep.bundles, []);
    let [bundle] = sweep.bundle_traits.as_slice() else {
        panic!("{:?}", sweep.bundle_traits)
    };
    assert_eq!(bundle.impls, ["r_k3::RIfM", "r_k5::RIfM"]);
    let bus = sweep
        .ports
        .methods
        .iter()
        .find(|m| m.name == "bus")
        .unwrap();
    let MethodKind::Bundle(TraitType::Assoc(i)) = &bus.kind else {
        panic!("{bus:?}")
    };
    assert_eq!(
        sweep.ports.types[*i].bound,
        AssocBound::Trait(bundle.name.clone())
    );
    // The interface's `clk` is alike; its `d` follows `W`.
    let concrete: Vec<&str> = bundle
        .methods
        .iter()
        .filter(|m| {
            matches!(
                m.kind,
                MethodKind::Signal {
                    ty: TraitType::Concrete(_),
                    ..
                }
            )
        })
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(concrete, ["clk"]);
}

#[test]
fn a_sweep_of_two_modules_is_refused() {
    let members = lowered(&[cases::sweep_member("a", 8, 2), cases::struct_pipe()]);
    let err = lower_sweep("mixed", &members).unwrap_err();
    assert!(
        matches!(&err, Error::Config { reason, .. } if reason.contains("`a` is `sw_top` and `struct_pipe` is `struct_pipe`")),
        "{err}"
    );
}

#[test]
fn a_member_named_like_a_shared_package_is_refused() {
    let members = lowered(&[
        cases::sweep_member("sw_pkg", 8, 2),
        cases::sweep_member("other", 9, 2),
    ]);
    let err = lower_sweep("sw", &members).unwrap_err();
    assert!(
        err.to_string().contains("DUT `sw_pkg` is named like"),
        "{err}"
    );
    let one = lowered(&[cases::sweep_member("sw", 8, 2)]);
    assert!(
        lower_sweep("sw", &one).is_err(),
        "a member named like the sweep"
    );
}

/// The committed sweep module, as `include_dut!(sw)` includes it.
pub mod sw {
    include!("generated/sweep/sw.rs");
}

/// The committed interface sweep.
pub mod r_sweep {
    include!("generated/iface_sweep/r_sweep.rs");
}

/// A body written once over every member, with no `where` clause: it names the handles
/// it was given, moves a value between the ports that share a type, and builds values.
fn body<D: sw::Ports>(dut: D) -> Vec<&'static str> {
    let _: In<Bit> = dut.clk();
    let _: In<sw::Hdr> = dut.hdr_in();
    let a: In<D::A> = dut.a();
    let _: In<D::A> = dut.b();
    let _: Out<D::Sum> = dut.sum();
    assert_eq!(<D::A as Default>::default(), D::A::from(0));
    assert!(D::W > 0);
    vec![
        dut.clk().meta.name,
        a.meta.name,
        dut.sum().meta.name,
        dut.word().meta.name,
        dut.lanes().meta.name,
    ]
}

fn vector<T: Vector>() {}
fn value<T: Value>() {}

#[test]
fn one_body_drives_every_member() {
    let small = sw::sw_small::Dut::__bind(&mut RecordingBinder::default());
    let big = sw::sw_big::Dut::__bind(&mut RecordingBinder::default());
    let names = [
        "sw_top.clk",
        "sw_top.a",
        "sw_top.sum",
        "sw_top.word",
        "sw_top.lanes",
    ];
    assert_eq!(body(small), names);
    assert_eq!(body(big), names);
    // The shared types are one Rust type in every module.
    let _: sw::Hdr = sw::sw_small::Hdr::default();
    let _: sw::sw_big::State = sw::State::IDLE;
    // The associated types are each member's own.
    vector::<<sw::sw_small::Dut as sw::Ports>::A>();
    assert_eq!(
        <<sw::sw_small::Dut as sw::Ports>::A>::default(),
        Bits::<8>::new(0)
    );
    assert_eq!(
        <<sw::sw_big::Dut as sw::Ports>::A>::default(),
        WideBits::<200, 7>::default()
    );
    value::<<sw::sw_big::Dut as sw::Ports>::Word>();
    assert_eq!((sw::sw_small::Dut::W, sw::sw_big::Dut::W), (8, 200));
}

fn bus<D: r_sweep::Ports>(dut: D) -> (&'static str, &'static str) {
    // A bundle's methods are the bundle trait's, which must be in scope.
    use r_sweep::RIfMPorts as _;
    (dut.bus().clk().meta.name, dut.bus().d().meta.name)
}

#[test]
fn a_bundle_trait_reaches_each_members_bundle() {
    let k3 = r_sweep::r_k3::Dut::__bind(&mut RecordingBinder::default());
    let k5 = r_sweep::r_k5::Dut::__bind(&mut RecordingBinder::default());
    assert_eq!(bus(k3), bus(k5));
    assert!(bus(k3).1.ends_with("bus_d"), "{:?}", bus(k3));
}

/// The fixpoint. `Mid` and `Outer` are alike in both members but reach `Inner`, which
/// follows `W` (`Outer` only through `Mid`, so it is dropped on the second pass); only the
/// parameter-free `Tag` and `Wrap` are shared.
#[test]
fn a_struct_reaching_an_unshared_type_through_another_is_not_shared() {
    let members = [
        cases::nested_sweep_member("nx_a", 8),
        cases::nested_sweep_member("nx_b", 4),
    ];
    let (sweep, _) = files("nx", &members);
    let shared: Vec<&str> = sweep.items.iter().map(|i| i.name.as_str()).collect();
    // Each module-local struct is named `NxTop<Name>` with a `<Name>` alias.
    assert_eq!(shared, ["NxTopTag", "Tag", "NxTopWrap", "Wrap"]);
}
