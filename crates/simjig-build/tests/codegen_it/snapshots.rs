//! Snapshots of every emitter's output for every case.

use simjig_build::XInitial;
use simjig_build::codegen::{
    Lowered, RustOptions, ShimOptions, emit_layout_check, emit_rust, emit_shim, emit_vlt,
    emit_wrapper, lower,
};

use crate::cases::{self, Case};

/// The model glue every case is emitted with: the prefix the shim snapshots use, and the
/// design's own precision.
fn rust_options(case: &Case, lowered: &Lowered) -> RustOptions {
    RustOptions {
        prefix: format!("V{}", lowered.top),
        precision_exponent: case.design.time_precision.0,
        dpi_imports: case
            .design
            .dpi_imports
            .iter()
            .map(|i| i.c_name.clone())
            .collect(),
    }
}

#[test]
fn emitter_snapshots() {
    for case in cases::all() {
        let lowered =
            lower(&case.design, &case.config).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        let name = &case.name;
        insta::assert_snapshot!(
            format!("{name}__rust"),
            emit_rust(&lowered, &rust_options(&case, &lowered))
        );
        insta::assert_snapshot!(format!("{name}__vlt"), emit_vlt(&lowered));
        insta::assert_snapshot!(format!("{name}__layout"), emit_layout_check(&lowered));
        if let Some(wrapper) = &lowered.wrapper {
            insta::assert_snapshot!(format!("{name}__wrapper"), emit_wrapper(wrapper));
        }
        let shim = emit_shim(
            &lowered,
            &ShimOptions {
                prefix: format!("V{}", lowered.top),
                timing: false,
                trace_fst: false,
                coverage: false,
                x_initial: XInitial::Unique,
            },
        );
        let shim = shim.unwrap_or_else(|e| format!("error: {e}"));
        insta::assert_snapshot!(format!("{name}__shim"), shim);
    }
}

/// The build options only change the traits' flags and, for coverage, the includes; one
/// case with probes and one without cover both include rules.
#[test]
fn shim_option_snapshots() {
    for case in [cases::wide(), cases::struct_pipe()] {
        let lowered = lower(&case.design, &case.config).unwrap();
        let name = &case.name;
        for (tag, timing, trace_fst, coverage) in [
            ("timing_trace", true, true, false),
            ("coverage", false, false, true),
        ] {
            let opts = ShimOptions {
                prefix: format!("V{}", lowered.top),
                timing,
                trace_fst,
                coverage,
                x_initial: XInitial::Unique,
            };
            let shim = emit_shim(&lowered, &opts).unwrap();
            insta::assert_snapshot!(format!("{name}__shim_{tag}"), shim);
        }
    }
}

/// A model whose initial values are compiled in (`--x-initial 0`/`fast`) says so with one
/// traits line; the default shim has none, so default snapshots are unchanged.
#[test]
fn only_a_compiled_in_x_initial_adds_a_traits_line() {
    let case = cases::wide();
    let lowered = lower(&case.design, &case.config).unwrap();
    let opts = |x_initial| ShimOptions {
        prefix: format!("V{}", lowered.top),
        timing: false,
        trace_fst: false,
        coverage: false,
        x_initial,
    };
    let line = "static constexpr bool kXInitialUnique = false;";
    let default = emit_shim(&lowered, &opts(XInitial::Unique)).unwrap();
    assert!(!default.contains("kXInitialUnique"), "{default}");
    let fast = emit_shim(&lowered, &opts(XInitial::Fast)).unwrap();
    assert_eq!(fast.replace(&format!("    {line}\n"), ""), default);
    assert!(fast.contains(line), "{fast}");
}

/// The generated Rust of every case is also committed under `generated/`, where
/// `compile.rs` includes it. Set `SIMJIG_BLESS=1` to rewrite the files after reviewing
/// the snapshot diff.
#[test]
fn committed_generated_rust_is_current() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/codegen_it/generated");
    let bless = std::env::var_os("SIMJIG_BLESS").is_some();
    for case in cases::all() {
        let lowered = lower(&case.design, &case.config).unwrap();
        let text = emit_rust(&lowered, &rust_options(&case, &lowered));
        let path = dir.join(format!("{}.rs", case.name));
        if bless {
            std::fs::write(&path, &text).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == text,
            "{} is stale; rerun with SIMJIG_BLESS=1",
            path.display()
        );
    }
}

/// A struct typedef'd inside an interface reaches the wrapper's flat ports spelled out
/// (anonymous), and is named after its typedef (`beat_t` is `Beat`), not after the first
/// flat port that carries it (`up_beat`).
#[test]
fn an_interface_typedef_names_the_struct_its_flat_ports_carry() {
    let case = cases::beat_sweep_member("beat_one", 8, 2);
    let lowered = lower(&case.design, &case.config).unwrap();
    let names: Vec<&str> = lowered.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["Beat"]);
    let beat = &lowered.bundles[0].members[0];
    assert_eq!(
        (beat.name.as_str(), &beat.ty),
        (
            "beat",
            &simjig_build::codegen::RustType::Named("Beat".into())
        )
    );
}
