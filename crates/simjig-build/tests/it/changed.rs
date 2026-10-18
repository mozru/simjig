//! An input edited while a tool reads it: the output serves that build, is not cached
//! (it would be found under the new contents' key), and the next build makes it again.
//! Each test edits an input from inside the tool's run, through a wrapper that edits it
//! once, right after the tool did its work.

use std::path::{Path, PathBuf};

use simjig_build::{Built, CacheStatus, CppRecipe, Dut};
use svtypes::ir::ParamValue;

use crate::support::{Sandbox, dut, write_script};

/// A wrapper that runs `"$@"` and, the first time an argument is `when`, writes `text` to
/// `edit` after the run.
fn editing_wrapper(s: &Sandbox, name: &str, run: &str, when: &str, edit: &Path, text: &str) {
    let flag = s.path().join(format!("{name}.edited"));
    let text_file = s.path().join(format!("{name}.text"));
    std::fs::write(&text_file, text).unwrap();
    write_script(
        &s.path().join(name),
        &format!(
            "#!/bin/sh\n{run} \"$@\"\nst=$?\nfor a in \"$@\"; do\n  case \"$a\" in\n    \
             {when}) if [ ! -e '{flag}' ]; then : > '{flag}'; cp '{text}' '{edit}'; fi;;\n  \
             esac\ndone\nexit $st\n",
            flag = flag.display(),
            text = text_file.display(),
            edit = edit.display(),
        ),
    );
}

fn warned(built: &Built, path: &Path, run: &str) -> bool {
    let path = format!("`{}` changed while {run}", path.display());
    built
        .directives
        .iter()
        .any(|d| d.starts_with("cargo::warning=") && d.contains(&path))
}

fn no_warning(built: &Built) -> bool {
    !built
        .directives
        .iter()
        .any(|d| d.starts_with("cargo::warning="))
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, Iterator::count)
}

/// The model archive the build links (from its link directives).
fn model_archive(built: &Built) -> PathBuf {
    let name = format!("model_{}", built.prefix);
    built
        .directives
        .iter()
        .filter_map(|d| d.strip_prefix("cargo::rustc-link-arg="))
        .map(PathBuf::from)
        .find(|p| p.file_name().unwrap().to_string_lossy().contains(&name))
        .expect("a model archive")
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

/// A header the model's C++ includes, rewritten after the compile that read it and
/// before the archive is cached.
#[test]
fn a_header_edited_while_compiling_is_not_cached() {
    let s = Sandbox::new();
    let inc = s.path().join("inc");
    std::fs::create_dir_all(&inc).unwrap();
    let header = inc.join("k.h");
    std::fs::write(&header, "#define NAME simjig_k_one\n").unwrap();
    let source = s.path().join("names_k.cpp");
    std::fs::write(
        &source,
        "#include \"k.h\"\nextern \"C\" int NAME() { return 0; }\n",
    )
    .unwrap();
    // The ccache stand-in runs every compile; it edits the header once `names_k.cpp` is
    // compiled.
    let edited = "#define NAME simjig_k_two\n";
    editing_wrapper(&s, "cxx-wrapper", "", "*names_k.cpp", &header, edited);
    let mut env = s.env();
    env.simjig_ccache = Some(s.path().join("cxx-wrapper").into());
    let build = || {
        Dut::new("inv")
            .sources([dut("inv").join("inv.sv")])
            .top("inv")
            .recipe(CppRecipe {
                model_sources: vec![source.clone()],
                include_dirs: vec![inc.clone()],
                ..CppRecipe::default()
            })
            .build_in(&env)
            .unwrap()
    };

    let first = build();
    assert!(
        warned(&first, &header, "compiling"),
        "{:?}",
        first.directives
    );
    let lib = std::fs::read(model_archive(&first)).unwrap();
    assert!(
        contains(&lib, "simjig_k_one"),
        "this build links what it compiled"
    );
    assert_eq!(
        entries(&s.path().join("cache/archives")),
        0,
        "nothing cached"
    );

    let second = build();
    assert!(no_warning(&second), "{:?}", second.directives);
    let lib = std::fs::read(model_archive(&second)).unwrap();
    assert!(
        contains(&lib, "simjig_k_two") && !contains(&lib, "simjig_k_one"),
        "the next build compiles the edited header"
    );
    assert_eq!(entries(&s.path().join("cache/archives")), 1);
    assert_eq!(
        model_archive(&build()),
        model_archive(&second),
        "then a hit"
    );
}

/// An included `.svh` rewritten right after Verilator verilated the model.
#[test]
fn an_include_edited_while_verilating_is_not_cached() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    let defs = rtl.join("defs.svh");
    let run = format!("'{}'", s.wrapper().display());
    editing_wrapper(&s, "vl-wrapper", &run, "--cc", &defs, "`define STEP 2\n");
    let mut env = s.env();
    env.simjig_verilator = Some(s.path().join("vl-wrapper"));
    let build = || {
        Dut::new("counter")
            .sources([rtl.join("counter.sv")])
            .include_dirs([&rtl])
            .top("counter")
            .build_in(&env)
            .unwrap()
    };

    let first = build();
    assert_eq!((first.cache, s.verilations()), (CacheStatus::Miss, 1));
    assert!(
        warned(&first, &defs, "verilating"),
        "{:?}",
        first.directives
    );
    assert!(warned(&first, &defs, "the front end ran"));
    assert!(first.obj_dir.join(format!("{}.h", first.prefix)).is_file());
    let unpublished = env.out_dir.join("simjig-unpublished");
    assert!(
        first.obj_dir.starts_with(&unpublished),
        "{:?}",
        first.obj_dir
    );
    assert_eq!(entries(&s.path().join("cache/models")), 0, "nothing cached");
    // Nor the model archive compiled from it: its sources' path names no entry.
    assert!(model_archive(&first).starts_with(&unpublished));
    assert_eq!(entries(&s.path().join("cache/archives")), 0);
    let first_text = model_text(&first);

    let second = build();
    assert_eq!(
        (second.cache, s.verilations()),
        (CacheStatus::Miss, 2),
        "the next build verilates the edited include"
    );
    assert!(no_warning(&second), "{:?}", second.directives);
    assert_ne!(first_text, model_text(&second));
    assert_eq!(entries(&s.path().join("cache/archives")), 1);
    let third = build();
    assert_eq!((third.cache, s.verilations()), (CacheStatus::Hit, 2));
    assert_eq!(third.obj_dir, second.obj_dir);
}

/// The C++ Verilator wrote, by file name.
fn model_text(built: &Built) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(&built.obj_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "cpp" || x == "h"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    files.sort();
    files
}

/// The value of the top module's `TAG` parameter in the build's IR.
fn tag(built: &Built) -> ParamValue {
    let top = &built.design.modules[usize::try_from(built.design.top.0).unwrap()];
    top.params
        .iter()
        .find(|p| p.name == "TAG")
        .expect("a TAG parameter")
        .value
        .clone()
}

/// An include rewritten right after the JSON pass read it: the IR is the old contents',
/// and must not be cached under the new ones.
#[test]
fn an_include_edited_during_the_json_pass_is_not_cached() {
    let s = Sandbox::new();
    let rtl = s.path().join("labelled");
    std::fs::create_dir_all(&rtl).unwrap();
    let tag_h = rtl.join("tag.svh");
    std::fs::write(&tag_h, "`define TAG \"one\"\n").unwrap();
    std::fs::write(
        rtl.join("labelled.sv"),
        "`include \"tag.svh\"\nmodule labelled #(parameter string TAG = `TAG) \
         (input logic a, output logic y);\n  assign y = a;\nendmodule\n",
    )
    .unwrap();
    let run = format!("'{}'", s.wrapper().display());
    editing_wrapper(
        &s,
        "vl-wrapper",
        &run,
        "--json-only",
        &tag_h,
        "`define TAG \"two\"\n",
    );
    let mut env = s.env();
    env.simjig_verilator = Some(s.path().join("vl-wrapper"));
    let build = || {
        Dut::new("labelled")
            .sources([rtl.join("labelled.sv")])
            .include_dirs([&rtl])
            .top("labelled")
            .build_in(&env)
            .unwrap()
    };

    let first = build();
    assert_eq!(tag(&first), ParamValue::String("one".into()));
    assert!(
        warned(&first, &tag_h, "the front end ran"),
        "{:?}",
        first.directives
    );
    assert_eq!(entries(&s.path().join("cache/fronts")), 0, "nothing cached");

    let second = build();
    assert_eq!(
        tag(&second),
        ParamValue::String("two".into()),
        "the next build reads the edited include"
    );
    assert!(no_warning(&second), "{:?}", second.directives);
    assert_eq!(entries(&s.path().join("cache/fronts")), 1);
}
