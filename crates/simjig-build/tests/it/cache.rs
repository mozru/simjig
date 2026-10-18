//! The content-hash cache: hits skip Verilator, edits to any input miss.

use simjig_build::{CacheStatus, Dut};

use crate::support::Sandbox;

fn counter(rtl: &std::path::Path, width: u32) -> Dut {
    Dut::new("counter")
        .sources([rtl.join("counter.sv")])
        .include_dirs([rtl])
        .param("WIDTH", width)
        .top("counter")
}

fn count_entries(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

#[test]
fn second_build_hits_the_cache_without_running_verilator() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let env = s.env();
    let first = counter(&rtl, 8).build_in(&env).unwrap();
    assert_eq!((first.cache, s.verilations()), (CacheStatus::Miss, 1));

    let second = counter(&rtl, 8).build_in(&env).unwrap();
    assert_eq!(
        (second.cache, s.verilations()),
        (CacheStatus::Hit, 1),
        "Verilator ran again"
    );
    assert_eq!(second.obj_dir, first.obj_dir);
    assert_eq!(
        second.directives, first.directives,
        "a hit emits the same directives"
    );

    // Another crate (another OUT_DIR) sharing the cache also hits.
    let mut other_crate = env.clone();
    other_crate.out_dir = s.path().join("other-out");
    let third = counter(&rtl, 8).build_in(&other_crate).unwrap();
    assert_eq!((third.cache, s.verilations()), (CacheStatus::Hit, 1));
    // And links the same model archive: the shim it compiles is content-addressed.
    assert_eq!(model_entry(&third), model_entry(&first));
    assert_eq!(count_entries(&s.path().join("cache/archives")), 1);

    // A different parameter is a different model, but reuses the same libverilated.
    // (Another crate: one build script may not reuse a DUT name for another config.)
    let mut wide_crate = env.clone();
    wide_crate.out_dir = s.path().join("wide-out");
    let wide = counter(&rtl, 16).build_in(&wide_crate).unwrap();
    assert_eq!((wide.cache, s.verilations()), (CacheStatus::Miss, 2));
    assert_ne!(wide.prefix, first.prefix);
    assert_eq!(count_entries(&s.path().join("cache/libverilated")), 1);
    assert_eq!(
        count_entries(&s.path().join("cache/tmp")),
        0,
        "work dirs are cleaned up"
    );
}

#[test]
fn editing_an_included_file_misses_and_reverting_hits() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let mut env = s.env();
    env.simjig_ccache = Some("0".into());
    let defs = rtl.join("defs.svh");
    let original = std::fs::read_to_string(&defs).unwrap();

    let first = counter(&rtl, 8).build_in(&env).unwrap();
    assert!(
        !s.path().join("out/simjig-cxx-ccache").exists(),
        "SIMJIG_CCACHE=0 disables ccache"
    );
    std::fs::write(&defs, "`define STEP 2\n").unwrap();
    let edited = counter(&rtl, 8).build_in(&env).unwrap();
    assert_eq!(
        (edited.cache, s.verilations()),
        (CacheStatus::Miss, 2),
        "the include changed"
    );
    assert_ne!(first.obj_dir, edited.obj_dir);
    assert_eq!(
        first.prefix, edited.prefix,
        "contents do not change the prefix"
    );

    std::fs::write(&defs, original).unwrap();
    let reverted = counter(&rtl, 8).build_in(&env).unwrap();
    assert_eq!((reverted.cache, s.verilations()), (CacheStatus::Hit, 2));
    assert_eq!(reverted.obj_dir, first.obj_dir);

    // A new file in an include directory could shadow an include: it must miss.
    std::fs::write(rtl.join("new.svh"), "").unwrap();
    let shadowed = counter(&rtl, 8).build_in(&env).unwrap();
    assert_eq!((shadowed.cache, s.verilations()), (CacheStatus::Miss, 3));
}

#[test]
fn a_dut_name_is_one_configuration_per_build_script() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let env = s.env();
    counter(&rtl, 8).build_in(&env).unwrap();
    let err = counter(&rtl, 16).build_in(&env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid DUT `counter` configuration: a DUT of this name was already built with a \
         different configuration in this build script; give each configuration its own name"
    );
}

#[test]
fn a_prefix_owned_by_another_configuration_is_rejected() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let dut = counter(&rtl, 8);
    let prefix = dut.config().prefix();
    // The claims live under a directory naming the identity scheme; find this one's.
    counter(&rtl, 8).build_in(&s.env()).unwrap();
    let verilated = s.verilations();
    let claims: Vec<_> = std::fs::read_dir(s.path().join("cache/prefixes"))
        .unwrap()
        .map(|scheme| scheme.unwrap().path().join(&prefix))
        .filter(|claim| claim.is_file())
        .collect();
    assert_eq!(claims.len(), 1, "{claims:?}");
    // What a hash collision with another crate's configuration leaves in the cache.
    std::fs::write(&claims[0], "someone-else").unwrap();
    let err = dut.build_in(&s.env()).unwrap_err();
    assert!(
        err.to_string().contains(&format!(
            "model prefix `{prefix}` is already used by a different configuration (identity \
             someone-else"
        )),
        "{err}"
    );
    assert_eq!(s.verilations(), verilated, "rejected before verilating");
}

/// An upgrade of simjig-build that changes `DutConfig` (so every identity's `Debug` form)
/// must not find its own old claims and fail with "rename the DUT": claims are kept per
/// identity scheme, and the claims an older simjig-build left (under its scheme, or in the
/// flat `prefixes/<prefix>` of the releases before schemes) are not consulted.
#[test]
fn an_upgrade_that_changes_the_config_shape_ignores_the_old_claims() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let dut = counter(&rtl, 8);
    let prefix = dut.config().prefix();
    let prefixes = s.path().join("cache/prefixes");
    // What the same configuration left as seen by older builds: another identity.
    let old_scheme = prefixes.join("0000000000000000");
    std::fs::create_dir_all(&old_scheme).unwrap();
    std::fs::write(old_scheme.join(&prefix), "identity-under-the-old-shape").unwrap();
    std::fs::write(prefixes.join(&prefix), "identity-before-schemes").unwrap();
    dut.build_in(&s.env()).unwrap();
    let claims: Vec<_> = std::fs::read_dir(&prefixes)
        .unwrap()
        .map(|scheme| scheme.unwrap().path().join(&prefix))
        .filter(|claim| claim.is_file())
        .collect();
    assert_eq!(
        claims.len(),
        2,
        "the old claim and this scheme's: {claims:?}"
    );
}

/// The model archive's entry directory (from its link directives).
fn model_entry(built: &simjig_build::Built) -> std::path::PathBuf {
    built
        .directives
        .iter()
        .filter_map(|d| d.strip_prefix("cargo::rustc-link-search=native="))
        .map(std::path::PathBuf::from)
        .find(|d| d.components().any(|c| c.as_os_str() == "archives"))
        .expect("a model archive directory")
}

/// An archive entry holds only the archive: its objects are inside it.
#[test]
fn archive_entries_hold_only_the_archive() {
    let s = Sandbox::new();
    let built = counter(&s.rtl(), 8).build_in(&s.env()).unwrap();
    for kind in ["archives", "libverilated"] {
        for entry in std::fs::read_dir(s.path().join("cache").join(kind)).unwrap() {
            let files: Vec<std::path::PathBuf> = std::fs::read_dir(entry.unwrap().path())
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert!(
                files.len() == 1 && files[0].extension().is_some_and(|e| e == "a"),
                "{kind}: {files:?}"
            );
        }
    }
    assert!(model_entry(&built).is_dir());
}

/// An entry directory without its archive (an interrupted older build) is a miss, and
/// the build puts the archive back instead of linking a missing file.
#[test]
fn an_entry_without_its_archive_is_rebuilt() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let built = counter(&rtl, 8).build_in(&s.env()).unwrap();
    let entry = model_entry(&built);
    for f in std::fs::read_dir(&entry).unwrap() {
        std::fs::remove_file(f.unwrap().path()).unwrap();
    }
    let again = counter(&rtl, 8).build_in(&s.env()).unwrap();
    assert_eq!(model_entry(&again), entry);
    let archives = std::fs::read_dir(&entry).unwrap().count();
    assert_eq!(archives, 1, "the archive is back");
}

/// Work directories left by a build that was killed are removed when they are an hour
/// old; younger ones may belong to a build still running.
#[test]
fn stale_work_dirs_are_removed() {
    let s = Sandbox::new();
    let tmp = s.path().join("cache/tmp");
    let (old, young) = (tmp.join("old"), tmp.join("young"));
    for d in [&old, &young] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("x.o"), "").unwrap();
    }
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    std::fs::File::open(&old)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();
    counter(&s.rtl(), 8).build_in(&s.env()).unwrap();
    assert!(!old.exists(), "the stale work dir is gone");
    assert!(young.exists(), "a recent work dir is kept");
}

/// Verilator 5.052's `.d` lists the option files it read (`-f`, `-F`), nested ones too, so
/// an option file no argument names is part of the key: editing it verilates again.
#[test]
fn editing_a_nested_option_file_misses() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let (outer, inner) = (s.path().join("outer.f"), s.path().join("inner.f"));
    std::fs::write(&outer, format!("-f {}\n", inner.display())).unwrap();
    std::fs::write(&inner, "-Wno-WIDTH\n").unwrap();
    let build = || {
        counter(&rtl, 8)
            .verilator_args(["-f".to_owned(), outer.display().to_string()])
            .build_in(&s.env())
            .unwrap()
    };
    let first = build();
    assert!(
        first
            .directives
            .contains(&format!("cargo::rerun-if-changed={}", inner.display())),
        "{:?}",
        first.directives
    );
    assert_eq!((build().cache, s.verilations()), (CacheStatus::Hit, 1));
    std::fs::write(&inner, "-Wno-UNUSED\n").unwrap();
    assert_eq!((build().cache, s.verilations()), (CacheStatus::Miss, 2));
}

/// `dpi_in_c` changes only the generated Rust, so it is not part of a DUT's prefix; it
/// must not be part of the identity its prefix claim records either, or moving an import
/// from Rust to C (an edit to the build script) finds the previous build's claim and fails
/// with "rename the DUT". Another `OUT_DIR` stands in for the next run of the build
/// script (the per-script name check is per process). From the s3c4 review.
#[test]
fn s3c4m_moving_a_dpi_import_to_c_is_not_a_prefix_collision() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    std::fs::write(rtl.join("imp.sv"), IMPORTS_F).unwrap();
    let dut = || Dut::new("imp").sources([rtl.join("imp.sv")]).top("imp");
    let first = dut().build_in(&s.env()).unwrap();
    let mut next_run = s.env();
    next_run.out_dir = s.path().join("next-out");
    let second = dut().dpi_in_c(["f"]).build_in(&next_run);
    let second = second.unwrap_or_else(|e| panic!("moving `f` to C failed the build: {e}"));
    assert_eq!(first.prefix, second.prefix, "the same model");
}

/// A design with one DPI-C import, `f`.
const IMPORTS_F: &str = "module imp(input logic [31:0] a, output logic [31:0] y);\n  \
    import \"DPI-C\" pure function int f(input int x);\n  assign y = f(a);\nendmodule\n";

/// Within one build script, the DUT name names the generated Rust, which `dpi_in_c`
/// changes: rebuilding a name with another `dpi_in_c` is refused although the model (and
/// its prefix) is the same.
#[test]
fn a_dut_name_rebuilt_with_another_dpi_in_c_is_refused() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    std::fs::write(rtl.join("imp.sv"), IMPORTS_F).unwrap();
    let dut = || Dut::new("imp").sources([rtl.join("imp.sv")]).top("imp");
    dut().build_in(&s.env()).unwrap();
    let err = dut().dpi_in_c(["f"]).build_in(&s.env()).unwrap_err();
    assert!(
        err.to_string()
            .contains("a DUT of this name was already built with a different configuration"),
        "{err}"
    );
}

/// A build that fails a check of the front end (here `dpi_in_c` naming an import the
/// design lacks) records no prefix claim in the shared cache.
#[test]
fn a_build_that_fails_its_checks_leaves_no_prefix_claim() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    std::fs::write(rtl.join("imp.sv"), IMPORTS_F).unwrap();
    let dut = Dut::new("imp")
        .sources([rtl.join("imp.sv")])
        .top("imp")
        .dpi_in_c(["g"]);
    let prefix = dut.config().prefix();
    let err = dut.build_in(&s.env()).unwrap_err();
    assert!(err.to_string().contains("`dpi_in_c` names `g`"), "{err}");
    let prefixes = s.path().join("cache/prefixes");
    let claims: Vec<_> = std::fs::read_dir(&prefixes)
        .into_iter()
        .flatten()
        .map(|scheme| scheme.unwrap().path().join(&prefix))
        .filter(|claim| claim.is_file())
        .collect();
    assert!(claims.is_empty(), "{claims:?}");
}
