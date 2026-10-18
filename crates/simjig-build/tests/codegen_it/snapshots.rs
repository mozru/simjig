//! Snapshots of every emitter's output for every case.

use simjig_build::codegen::{
    ShimOptions, emit_layout_check, emit_rust, emit_shim, emit_vlt, emit_wrapper, lower,
};

use crate::cases;

#[test]
fn emitter_snapshots() {
    for case in cases::all() {
        let lowered =
            lower(&case.design, &case.config).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        let name = &case.name;
        insta::assert_snapshot!(format!("{name}__rust"), emit_rust(&lowered));
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
            },
        );
        let shim = shim.unwrap_or_else(|e| format!("error: {e}"));
        insta::assert_snapshot!(format!("{name}__shim"), shim);
    }
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
        let text = emit_rust(&lowered);
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
