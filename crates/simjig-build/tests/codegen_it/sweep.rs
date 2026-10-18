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
            ("A", &AssocBound::Vector, vec!["a", "b", "word.data"]),
            ("Sum", &AssocBound::Vector, vec!["sum"]),
            (
                "Word",
                &AssocBound::Trait("WordFields".into()),
                vec!["word"]
            ),
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

/// A struct whose field widths follow the parameters, carried by two interface ports of
/// two modports (`up.beat`, `down.beat`): one associated type `Beat` in `Ports`, named
/// after the typedef, that both bundle traits' `Beat` are fixed to, bounded by a
/// `BeatFields` trait over its fields.
#[test]
fn a_struct_that_differs_gets_one_type_and_a_fields_trait() {
    let cases = [
        cases::beat_sweep_member("bt_a", 8, 2),
        cases::beat_sweep_member("bt_b", 16, 4),
    ];
    let (sweep, files) = files("bt", &cases);
    check_generated("beat_sweep", &files);
    let t = &sweep.ports;
    let names: Vec<(&str, &AssocBound, Vec<&str>)> = t
        .types
        .iter()
        .map(|a| {
            let users = a.users.iter().map(String::as_str).collect();
            (a.name.as_str(), &a.bound, users)
        })
        .collect();
    let fields = AssocBound::Trait("BeatFields".into());
    assert_eq!(
        names,
        [
            ("PeekIn", &AssocBound::Vector, vec!["peek_in", "peek_out"]),
            ("Beat", &fields, vec!["up.beat", "down.beat"]),
            (
                "Data",
                &AssocBound::Vector,
                vec!["up.beat.data", "down.beat.data"]
            ),
            (
                "Tag",
                &AssocBound::Vector,
                vec!["up.beat.tag", "down.beat.tag"]
            ),
            (
                "Up",
                &AssocBound::Trait("BeatIfSinkPorts".into()),
                vec!["up"]
            ),
            (
                "Down",
                &AssocBound::Trait("BeatIfSourcePorts".into()),
                vec!["down"]
            ),
        ]
    );
    // The bundle traits' and the fields trait's types are `Ports`' (indices into `names`).
    let fixed = [("Data".to_owned(), 2), ("Tag".to_owned(), 3)];
    assert_eq!(t.types[1].equals, fixed);
    let bundle = [("Beat".to_owned(), 1), fixed[0].clone(), fixed[1].clone()];
    assert_eq!(t.types[4].equals, bundle);
    assert_eq!(t.types[5].equals, bundle);
    let [beat] = sweep.fields_traits.as_slice() else {
        panic!("{:?}", sweep.fields_traits)
    };
    assert_eq!(beat.impls, ["bt_a::Beat", "bt_b::Beat"]);
    let fields: Vec<(&str, &str, &str)> = beat
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.getter.as_str(), f.setter.as_str()))
        .collect();
    assert_eq!(
        fields,
        [("data", "data", "with_data"), ("tag", "tag", "with_tag")]
    );
    assert_eq!(sweep.runtime_traits, ["Value", "Vector", "Packed"]);
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

/// The committed sweep over a struct whose field widths differ.
pub mod bt {
    include!("generated/beat_sweep/bt.rs");
}

/// A body generic over `bt` with one import: `up.beat` and `down.beat` are one type,
/// `D::Beat`, built with `new` and `with_tag` from plain integers, read field by field,
/// and handed to a handle's `set`-shaped `deposit` without naming its type.
mod beat_body {
    use super::bt::*;
    use crate::__private::{In, Out};

    /// The shape of `In::set`: `value: impl Into<T>`.
    fn deposit<T>(_: In<T>, value: impl Into<T>) -> T {
        value.into()
    }

    pub(super) fn body<D: Ports>(dut: D) -> (u128, u128, u128, u32) {
        let up: In<D::Beat> = dut.up().beat();
        let _: Out<D::Beat> = dut.down().beat();
        let beat = D::Beat::new(0x5a, 1).with_tag(2);
        let moved = deposit(up, beat);
        assert_eq!(moved, beat);
        assert_eq!(D::Beat::from_u128(beat.to_u128()), beat);
        (
            beat.data().to_u128(),
            beat.tag().to_u128(),
            beat.to_u128(),
            <D::Beat as Packed>::WIDTH,
        )
    }
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

/// A trait method named like a supertrait's (`Clone::clone`; `PartialEq::eq` and another
/// field's setter `with_x` for a fields trait) gets a `_` appended, and its docs say why.
#[test]
fn a_method_named_like_a_supertraits_gets_an_underscore() {
    let members = [
        cases::nested_sweep_member("nx_a", 8),
        cases::nested_sweep_member("nx_b", 4),
    ];
    let (sweep, files) = files("nx", &members);
    let clone = sweep
        .ports
        .methods
        .iter()
        .find(|m| m.field == "clone")
        .unwrap();
    assert_eq!(clone.name, "clone_");
    let odd = sweep
        .fields_traits
        .iter()
        .find(|t| t.name == "OddFields")
        .unwrap();
    let methods: Vec<(&str, &str)> = odd
        .fields
        .iter()
        .map(|f| (f.getter.as_str(), f.setter.as_str()))
        .collect();
    assert_eq!(
        methods,
        [
            ("eq_", "with_eq"),
            ("with_x_", "with_with_x"),
            ("x", "with_x")
        ]
    );
    let text = &files[0].1;
    assert!(text.contains("{ self.clone }"), "{text}");
    assert!(
        text.contains("(`clone_` here: a supertrait or a blanket impl has a method `clone`)"),
        "{text}"
    );
}

#[test]
fn a_fields_trait_reads_and_builds_each_members_struct() {
    let a = bt::bt_a::Dut::__bind(&mut RecordingBinder::default());
    let b = bt::bt_b::Dut::__bind(&mut RecordingBinder::default());
    // `data` is the high field: `W` bits above `T` bits of `tag`.
    assert_eq!(beat_body::body(a), (0x5a, 2, 0x5a << 2 | 2, 10));
    assert_eq!(beat_body::body(b), (0x5a, 2, 0x5a << 4 | 2, 20));
    let one: bt::bt_a::Beat = <bt::bt_a::Beat as bt::BeatFields>::new(1, 0);
    assert_eq!(one.data, Bits::new(1));
}

/// The index of `Ports`' associated type `name`.
fn assoc_index(t: &simjig_build::codegen::PortsTrait, name: &str) -> usize {
    t.types.iter().position(|a| a.name == name).unwrap()
}

/// A port and a struct field of one type in every member (`dout`, `pa.beat.data`), and a
/// struct port nesting another's type (`o.i`, `io`), are one type in a generic body: the
/// fields trait's associated types are fixed to `Ports`' as a bundle trait's are
/// (`type AIfBeatT: AIfBeatTFields<Data = Self::Din, ..>`), or `dout == beat.data()` and
/// `io == o.i()` do not compile (E0277, E0308; `fields_body` checks they do).
#[test]
fn a_fields_traits_types_are_fixed_to_the_ports_traits() {
    let (sweep, out) = files(
        "fs",
        &[
            cases::fields_sweep_member("fs_a", 8),
            cases::fields_sweep_member("fs_b", 16),
        ],
    );
    check_generated("fields_sweep", &out);
    let t = &sweep.ports;
    // Every type of the fields trait is fixed, `Tag` too though no port has it.
    let fixed = [
        ("Data".to_owned(), assoc_index(t, "Din")),
        ("Tag".to_owned(), assoc_index(t, "Tag")),
    ];
    assert_eq!(t.types[assoc_index(t, "AIfBeatT")].equals, fixed);
    // The bundle trait's `beat` is that type, and its fields' types are too.
    let pa = &t.types[assoc_index(t, "Pa")].equals;
    assert_eq!(pa[0], ("AIfBeatT".to_owned(), assoc_index(t, "AIfBeatT")));
    assert_eq!(pa[1..], fixed);
    let (sweep, out) = files(
        "fn",
        &[
            cases::nested_fields_member("fn_a", 8),
            cases::nested_fields_member("fn_b", 16),
        ],
    );
    check_generated("fields_sweep", &out);
    let t = &sweep.ports;
    let outer = &t.types[assoc_index(t, "Outer")].equals;
    assert_eq!(*outer, [("Inner".to_owned(), assoc_index(t, "Inner"))]);
    // The nested struct's own field type is fixed through `Inner`'s bound.
    let inner = &t.types[assoc_index(t, "Inner")].equals;
    assert_eq!(*inner, [("A".to_owned(), assoc_index(t, "A"))]);
}

/// The committed sweeps of `cases/sweep_fields`.
pub mod fs {
    include!("generated/fields_sweep/fs.rs");
}

/// `fn` is a keyword, so the module is named for Rust.
pub mod fn_sweep {
    include!("generated/fields_sweep/fn.rs");
}

/// Bodies generic over `fs` and `fn_sweep` comparing a struct field with a port of the
/// same type, and a nested struct with a port of its type, with no `where` clause.
mod fields_body {
    use simjig_bits::Bit;

    use crate::__private::{In, Out};

    /// `pa.beat.data` and `pb.beat.x` have `dout`'s type: a field's value is the port's.
    pub(super) fn ports<D: super::fs::Ports>(dut: D) -> bool {
        use super::fs::*;
        let _: Out<D::Din> = dut.dout();
        let _: In<D::AIfBeatT> = dut.pa().beat();
        let beat = D::AIfBeatT::new(3, 1);
        let field: D::Din = beat.data();
        let other: D::Din = D::BIfBeatT::default().x();
        field == beat.data() && other != field
    }

    /// `o.i` has `io`'s type, and `rw.raw` that of `o.i.a`.
    pub(super) fn nested<D: super::fn_sweep::Ports>(dut: D) -> (bool, u128) {
        use super::fn_sweep::*;
        let _: Out<D::Inner> = dut.io();
        let inner = D::Inner::new(5);
        let outer = D::Outer::new(inner, 1);
        let rw = D::Rw::new(outer.i().a(), 0, 1);
        let _: Out<Bit> = dut.from_();
        (inner == outer.i() && rw.into_() == 1, rw.raw().to_u128())
    }
}

#[test]
fn a_generic_body_compares_struct_fields_with_ports() {
    let binder = &mut RecordingBinder::default();
    assert!(fields_body::ports(fs::fs_a::Dut::__bind(binder)));
    assert!(fields_body::ports(fs::fs_b::Dut::__bind(binder)));
    let nested = (true, 5);
    assert_eq!(
        fields_body::nested(fn_sweep::fn_a::Dut::__bind(binder)),
        nested
    );
    assert_eq!(
        fields_body::nested(fn_sweep::fn_b::Dut::__bind(binder)),
        nested
    );
}

/// A fields trait's associated type is not named like one of its supertraits'
/// (`Packed::Raw`): `Self::Raw` would be ambiguous (E0221) and the sweep would not compile.
#[test]
fn a_fields_traits_types_are_not_named_like_a_supertraits() {
    let (sweep, _) = files(
        "fn",
        &[
            cases::nested_fields_member("fn_a", 8),
            cases::nested_fields_member("fn_b", 16),
        ],
    );
    let rw = sweep
        .fields_traits
        .iter()
        .find(|f| f.name == "RwFields")
        .unwrap();
    let names: Vec<&str> = rw.types.iter().map(|a| a.name.as_str()).collect();
    assert!(!names.contains(&"Raw"), "{names:?}");
}

/// A field or port named like a blanket impl's method (`Into::into`, `From::from`) gets a
/// `_` appended, or `x.into()` is ambiguous (E0034): `rw.into_()`, `dut.from_()`.
#[test]
fn a_method_named_like_a_blanket_impls_gets_an_underscore() {
    let (sweep, _) = files(
        "fn",
        &[
            cases::nested_fields_member("fn_a", 8),
            cases::nested_fields_member("fn_b", 16),
        ],
    );
    let rw = sweep
        .fields_traits
        .iter()
        .find(|f| f.name == "RwFields")
        .unwrap();
    let into = rw.fields.iter().find(|f| f.name == "into").unwrap();
    assert_eq!(
        (into.getter.as_str(), into.setter.as_str()),
        ("into_", "with_into")
    );
    let from = sweep
        .ports
        .methods
        .iter()
        .find(|m| m.field == "from")
        .unwrap();
    assert_eq!(from.name, "from_");
}

/// Structs nested three deep, one struct at two depths and as a port, and field types
/// shared with flat ports generate (and, included below, compile).
#[test]
fn deep_nested_struct_sweeps_generate() {
    let (_, out) = files(
        "dp",
        &[
            cases::deep_iface_member("dp_a", 8),
            cases::deep_iface_member("dp_b", 16),
        ],
    );
    check_generated("deep_sweep", &out);
    let (_, out) = files(
        "dq",
        &[
            cases::deep_member("dq_a", 8),
            cases::deep_member("dq_b", 16),
        ],
    );
    check_generated("deep_sweep", &out);
}

/// The committed sweep `dp`: two interface ports over a nested struct.
pub mod dp {
    include!("generated/deep_sweep/dp.rs");
}
/// The committed sweep `dq`: a struct nested three deep.
pub mod dq {
    include!("generated/deep_sweep/dq.rs");
}

/// Generic bodies relating fields three levels deep to ports and to each other: each
/// comparison compiles only if the sweep makes the two sides one associated type.
mod deep_body {
    use crate::__private::{In, Out};

    pub(super) fn dq<D: super::dq::Ports>(dut: D) -> bool {
        use super::dq::*;
        let _: In<D::Top> = dut.top();
        let _: Out<D::Leaf> = dut.lf();
        let _: In<D::A> = dut.din();
        let _: Out<D::B> = dut.bout();
        let top = D::Top::default();
        let deep: D::A = top.m().x().a();
        let b: D::B = top.m().b();
        let leaf: D::Leaf = top.l();
        top.a2() == deep && leaf == top.m().y() && leaf.a() == deep && b == D::B::default()
    }

    pub(super) fn dp<D: super::dp::Ports>(dut: D) -> bool {
        use super::dp::*;
        let _: In<D::Din> = dut.din();
        let _: In<D::Mid> = dut.p().beat();
        let _: In<D::Din> = dut.q().side();
        let mid = D::Mid::default();
        let a: D::Din = mid.x().a();
        let b: D::Bout = mid.b();
        a == mid.y().a() && b == D::Bout::default()
    }
}

#[test]
fn deep_bodies_relate_fields_and_ports() {
    let binder = &mut RecordingBinder::default();
    assert!(deep_body::dq(dq::dq_a::Dut::__bind(binder)));
    assert!(deep_body::dq(dq::dq_b::Dut::__bind(binder)));
    assert!(deep_body::dp(dp::dp_a::Dut::__bind(binder)));
    assert!(deep_body::dp(dp::dp_b::Dut::__bind(binder)));
}
