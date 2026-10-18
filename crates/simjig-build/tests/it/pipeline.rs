//! The whole pipeline in a simulated build-script environment.

use std::path::Path;

use simjig_build::svtypes::ir::Design;
use simjig_build::{
    Built, CacheStatus, Dut, DutConfig, Emitted, Error, HeaderPort, PortDir, Storage,
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
        storage: Storage::for_width(msb + 1),
    };
    let want = [
        port("clk", PortDir::In, 0),
        port("rst_n", PortDir::In, 0),
        port("count", PortDir::Out, 11),
    ];
    assert_eq!(built.header_ports, want, "WIDTH=12 reached Verilator");
    check_links(&s, &built);
    check_reruns(&rtl, &built.directives);

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
    assert_eq!(links[0], format!("static=simjig_model_{}", built.prefix));
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
        "PATH",
        "SIMJIG_CXX_OPT",
        "VERILATOR_ROOT",
        "CXX",
        "SIMJIG_VERILATOR",
        "SIMJIG_CACHE_DIR",
    ] {
        let line = format!("cargo::rerun-if-env-changed={var}");
        assert!(directives.contains(&line), "{var}");
    }
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
            storage: Storage::C8,
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
            "`count`: expected Out count[7:0] as C8, header declares Out count[11:0] as S16"
        ),
        "{msg}"
    );
    assert!(
        msg.contains("not expected: header declares In clk[0:0] as C8"),
        "{msg}"
    );
}
