//! Compiling Verilator's C++ output into static libraries and linking them, following
//! the C ABI's compile recipe (docs/notes/abi.md).
//!
//! Every translation unit gets the same base: `-include simjig_vl_config.h` (the
//! `VL_USER_*` hooks, `VL_PRINTF` redirection and `VL_TIME_CONTEXT`), `-std=gnu++20`
//! (coroutines, for `--timing`), `-O<cxx_opt>` (see [`BuildEnv::cxx_opt`]), the `VM_*`
//! defines, and include paths for the runtime headers and Verilator's `include/`.
//!
//! Libraries, in link order:
//! 1. **the model**: the generated classes from `<prefix>_classes.mk`, the emitted shim
//!    and [`CppRecipe::model_sources`]. Its `VM_*` defines are the model's own switches
//!    from `_classes.mk`, exactly as `verilated.mk` passes them. Cached in the shared
//!    cache like the runtime (`archives/<key>/`), keyed on the Verilator output directory
//!    (content-addressed), the shim's and recipe sources' contents, every header they
//!    read and the toolchain: a build script reruns for any edit to any of its DUTs, and
//!    only the edited DUT's model is compiled again.
//! 2. **runtime extras**: runtime classes that only some models need (`verilated_fst_c`
//!    for `--trace-fst`, `verilated_cov` for `--coverage`, `verilated_covergroup` for
//!    covergroups), one cached archive per class and set of switches, compiled with the
//!    switches of the model that asked for it. Two models of one binary may therefore
//!    link two archives of one class, defining the same symbols; a static link keeps the
//!    first. That is harmless only while the copies are the same code: of 5.052's extras,
//!    `verilated_fst_c.cpp` and `verilated_cov.cpp` read no `VM_*` switch, but
//!    `verilated_covergroup.cpp` reads `VM_COVERAGE`, so two covergroup models of one
//!    build script must agree on `--coverage` (`script::claim_covergroups` enforces it).
//! 3. **the shared libverilated**: `verilated.cpp`, `verilated_threads.cpp`,
//!    `verilated_timing.cpp`, `simjig_rt.cpp` and [`CppRecipe::global_sources`], with
//!    fixed `VM_*` defines ([`RUNTIME_SWITCHES`]). Fixed, so that every model of a
//!    configuration links the *same* archive: two archives defining the same runtime
//!    symbols in one binary would be an ODR violation. That is sound because these
//!    sources of 5.052 read only `VM_SC`, `VM_VPI` and `VM_SOLVER_DEFAULT` (unset; and
//!    `VM_TRACE` for a compatibility macro), and generated code reads no `VM_*` at all
//!    (checked with grep, abi.md).
//!    Cached once per (Verilator version, compiler, target, flags, runtime sources);
//!    the runtime sources include `simjig_vl_config.h`.
//! 4. native libraries the extras need (`lz4`, `z` for FST), then the C++ standard
//!    library.
//!
//! Each is emitted both as `rustc-link-lib` and as `rustc-link-arg` (see [`link_args`]),
//! so the models link into every target of the package, whether or not it has a library.
//!
//! Every archive is keyed on its sources and every file the compiler read for them, so a
//! header edit anywhere (next to a recipe source, in an include directory, in the
//! toolchain) compiles it again (see `archive`). The build script reruns on those files
//! too, except the ones under the cache and `OUT_DIR`, which derive from tracked inputs,
//! and on the compiler's executable (`archive::compiler_files`).
//!
//! ccache is used when available (see [`ccache_compiler`]).

use std::path::{Path, PathBuf};

use crate::archive::{self, Archive, Spec, Toolchain};
use crate::cache::Cache;
use crate::dut::DutConfig;
use crate::runtime::Runtime;
use crate::verilator::{self, Verilator};
use crate::{BuildEnv, Error, script};

/// Additions to the C ABI compile recipe. The default adds nothing, which *is* the ABI
/// recipe: the base flags, runtime and link order described in the module docs are
/// always applied, because every simjig model needs them (the generated shim includes
/// `simjig_model.h`, and the hooks in `simjig_rt.cpp` must be linked exactly once).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CppRecipe {
    /// Extra preprocessor defines for *every* translation unit, libverilated included. A
    /// `None` value means `-DNAME` without a value.
    pub defines: Vec<(String, Option<String>)>,
    /// Extra sources linked once per executable, compiled into the shared libverilated.
    pub global_sources: Vec<PathBuf>,
    /// Extra sources compiled with each model.
    pub model_sources: Vec<PathBuf>,
    /// Extra include directories for every translation unit.
    pub include_dirs: Vec<PathBuf>,
    /// Extra compiler flags for every translation unit.
    pub flags: Vec<String>,
}

/// The runtime classes in the shared libverilated, whatever the model's switches.
const BASE_RUNTIME: &[&str] = &["verilated", "verilated_threads", "verilated_timing"];

/// The `VM_*` defines of the shared libverilated (see the module docs). `VM_TIMING` is
/// on because `verilated_timing.cpp` is always in the archive.
pub(crate) const RUNTIME_SWITCHES: &[(&str, &str)] = &[
    ("VERILATOR", "1"),
    ("VM_COVERAGE", "0"),
    ("VM_SC", "0"),
    ("VM_TIMING", "1"),
    ("VM_TRACE", "0"),
    ("VM_TRACE_FST", "0"),
    ("VM_TRACE_VCD", "0"),
    ("VM_TRACE_SAIF", "0"),
    ("VM_VPI", "0"),
];

/// The C++ dialect of every translation unit: C++20 for `--timing` coroutines; GNU
/// extensions because Verilator's own makefiles use them.
const CXX_STD: &str = "gnu++20";

/// What `<prefix>_classes.mk` says about the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassesMk {
    /// `VM_*` switches (`VM_TRACE = 1` etc.), in file order.
    switches: Vec<(String, String)>,
    /// Model classes (`VM_CLASSES_*`, `VM_SUPPORT_*`), without `.cpp`.
    model: Vec<String>,
    /// Runtime classes (`VM_GLOBAL_*`), without `.cpp`.
    global: Vec<String>,
}

/// The switches `verilated.mk` turns into `-D` flags (`VK_CPPFLAGS_ALWAYS`).
const DEFINE_SWITCHES: &[&str] = &[
    "VM_COVERAGE",
    "VM_TIMING",
    "VM_TRACE",
    "VM_TRACE_FST",
    "VM_TRACE_VCD",
    "VM_TRACE_SAIF",
    "VM_VPI",
];

impl ClassesMk {
    pub(crate) fn parse(path: &Path, text: &str) -> Result<Self, Error> {
        let mut mk = Self {
            switches: Vec::new(),
            model: Vec::new(),
            global: Vec::new(),
        };
        let mut list: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some(name) = &list {
                if line.is_empty() {
                    list = None;
                    continue;
                }
                let item = line.trim_end_matches('\\').trim().to_owned();
                if name.starts_with("VM_GLOBAL_") {
                    mk.global.push(item);
                } else if name.starts_with("VM_CLASSES_") || name.starts_with("VM_SUPPORT_") {
                    mk.model.push(item);
                }
            } else if let Some(name) = line.strip_suffix("+= \\") {
                list = Some(name.trim().to_owned());
            } else if let Some((k, v)) = line.split_once(" = ") {
                mk.switches.push((k.trim().to_owned(), v.trim().to_owned()));
            }
        }
        for s in DEFINE_SWITCHES {
            if mk.switch(s).is_none() {
                return Err(Error::Parse {
                    path: path.to_path_buf(),
                    line: 0,
                    msg: format!("missing switch `{s}`"),
                });
            }
        }
        Ok(mk)
    }

    fn switch(&self, name: &str) -> Option<&str> {
        self.switches
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn on(&self, name: &str) -> bool {
        self.switch(name).is_some_and(|v| v != "0")
    }

    /// Runtime classes this model needs beyond [`BASE_RUNTIME`].
    fn extras(&self) -> impl Iterator<Item = &str> {
        self.global
            .iter()
            .map(String::as_str)
            .filter(|c| !BASE_RUNTIME.contains(c))
    }

    /// The `-D` flags `verilated.mk` passes to the model's translation units.
    /// (`VL_TIME_CONTEXT` comes from `simjig_vl_config.h`.)
    fn defines(&self) -> Vec<(String, Option<String>)> {
        let mut d = vec![
            ("VERILATOR".to_owned(), Some("1".to_owned())),
            ("VM_SC".to_owned(), Some("0".to_owned())),
        ];
        for s in DEFINE_SWITCHES {
            d.push(((*s).to_owned(), self.switch(s).map(str::to_owned)));
        }
        d
    }
}

pub(crate) struct Inputs<'a> {
    pub(crate) env: &'a BuildEnv,
    pub(crate) config: &'a DutConfig,
    pub(crate) cache: &'a Cache,
    pub(crate) verilator: &'a Verilator,
    pub(crate) prefix: &'a str,
    pub(crate) obj_dir: &'a Path,
    pub(crate) recipe: &'a CppRecipe,
    /// Generated sources compiled with the model (the emitted shim).
    pub(crate) model_sources: &'a [PathBuf],
}

/// The shim and the link were set up for [`DutConfig::features`]; Verilator's own
/// switches say what it built. They differ only when an argument simjig cannot read (an
/// option file, `-f`) changed tracing or coverage, and the build then stops here instead
/// of linking a shim that disagrees with its model.
fn check_features(config: &DutConfig, mk: &ClassesMk) -> Result<(), Error> {
    let want = config.features();
    for (switch, built, wanted, how) in [
        (
            "VM_COVERAGE",
            mk.on("VM_COVERAGE"),
            want.coverage,
            "`.coverage(..)` and the \
          `--coverage*` arguments",
        ),
        (
            "VM_TRACE_FST",
            mk.on("VM_TRACE_FST"),
            want.trace_fst,
            "`.trace_fst(..)`",
        ),
    ] {
        if built != wanted {
            return Err(Error::Config {
                dut: config.name.clone(),
                msg: format!(
                    "Verilator built the model with {switch} = {}, but {how} say {}; an \
                     option file (`-f`) must not change tracing or coverage",
                    u8::from(built),
                    u8::from(wanted)
                ),
            });
        }
    }
    Ok(())
}

/// Compiles the libraries (see the module docs) and returns the link directives in the
/// order a static linker needs: each library before the ones it calls.
pub(crate) fn compile(i: &Inputs<'_>) -> Result<Vec<String>, Error> {
    let mk_path = verilator::output_file(i.obj_dir, i.prefix, "_classes.mk");
    let text = std::fs::read_to_string(&mk_path).map_err(Error::io("reading", &mk_path))?;
    let mk = ClassesMk::parse(&mk_path, &text)?;
    check_features(i.config, &mk)?;
    if mk.global.iter().any(|c| c == "verilated_covergroup") {
        script::claim_covergroups(i.env, i.config, i.prefix, mk.on("VM_COVERAGE"))?;
    }
    let runtime = Runtime::extract(i.cache)?;
    let (base, compiler) = base_build(i, &runtime)?;
    let tc = Toolchain {
        i,
        base: &base,
        compiler: &compiler,
        identity: archive::compiler_identity(i.env, &compiler),
    };

    let mut model_defines = mk.defines();
    model_defines.extend(i.recipe.defines.iter().cloned());
    let mut model_sources: Vec<PathBuf> = mk
        .model
        .iter()
        .map(|c| i.obj_dir.join(format!("{c}.cpp")))
        .collect();
    model_sources.extend(i.model_sources.iter().cloned());
    model_sources.extend(i.recipe.model_sources.iter().cloned());
    let model = tc.cached_archive(&Spec {
        kind: "archives",
        what: &format!("model_{}", i.prefix),
        sources: &model_sources,
        defines: &model_defines,
        includes: &[i.obj_dir.to_path_buf()],
    })?;

    let natives = native_libs(i.env, &mk);
    let mut extras = Vec::new();
    for class in mk.extras() {
        let src = i.verilator.include_dir.join(format!("{class}.cpp"));
        let includes: Vec<PathBuf> = natives.iter().filter_map(NativeLib::include_dir).collect();
        extras.push(tc.cached_archive(&Spec {
            kind: "libverilated",
            what: class,
            sources: &[src],
            defines: &model_defines,
            includes: &includes,
        })?);
    }

    let shared = shared_runtime(&tc, &runtime)?;
    let archives: Vec<&Archive> = std::iter::once(&model)
        .chain(&extras)
        .chain([&shared])
        .collect();
    let stdlib = cxx_stdlib(i.env);
    let mut d = Vec::new();
    for a in &archives {
        d.push(format!(
            "cargo::rustc-link-search=native={}",
            a.dir.display()
        ));
        d.push(format!("cargo::rustc-link-lib=static={}", a.name));
    }
    for n in &natives {
        d.extend(n.directives());
    }
    d.extend(stdlib.iter().map(|l| format!("cargo::rustc-link-lib={l}")));
    d.extend(link_args(&archives, &natives, stdlib.as_deref()));
    d.extend(rerun_on_deps(i, &archives));
    d.extend(
        archive::compiler_files(i.env, &compiler)
            .into_iter()
            .map(|p| format!("cargo::rerun-if-changed={}", p.display())),
    );
    Ok(d)
}

/// The shared libverilated, with its fixed switches (see the module docs).
fn shared_runtime(tc: &Toolchain<'_>, runtime: &Runtime) -> Result<Archive, Error> {
    let i = tc.i;
    let mut defines: Vec<(String, Option<String>)> = RUNTIME_SWITCHES
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Some((*v).to_owned())))
        .collect();
    defines.extend(i.recipe.defines.iter().cloned());
    let mut sources: Vec<PathBuf> = BASE_RUNTIME
        .iter()
        .map(|c| i.verilator.include_dir.join(format!("{c}.cpp")))
        .collect();
    sources.push(runtime.source());
    sources.extend(i.recipe.global_sources.iter().cloned());
    tc.cached_archive(&Spec {
        kind: "libverilated",
        what: "verilated",
        sources: &sources,
        defines: &defines,
        includes: &[],
    })
}

/// `rerun-if-changed` for every file the compiler read, except the ones under the cache
/// and `OUT_DIR`: those derive from tracked inputs, and this run may have just written
/// them, which Cargo would take as a change on the next build.
fn rerun_on_deps(i: &Inputs<'_>, archives: &[&Archive]) -> Vec<String> {
    let mut deps: Vec<&PathBuf> = archives
        .iter()
        .flat_map(|a| &a.deps)
        .filter(|p| !p.starts_with(i.cache.root()) && !p.starts_with(&i.env.out_dir))
        .collect();
    deps.sort();
    deps.dedup();
    deps.into_iter()
        .map(|p| format!("cargo::rerun-if-changed={}", p.display()))
        .collect()
}

/// The same libraries again, as `cargo::rustc-link-arg` lines, in link order.
///
/// Why both: in a package with a library target, Cargo passes `rustc-link-lib` to the
/// library only. The rlib bundles the archives, but a test, example or binary that
/// `include!`s the generated `Dut` without naming the library never links that rlib, and
/// the vtable symbol is undefined. `rustc-link-arg` reaches every linked target of the
/// package (tests, examples, benches, binaries, cdylib), and only those, while
/// `rustc-link-lib` still carries the archives into the rlib for crates that depend on
/// the library. Listing an archive twice links each object at most once (a static
/// linker pulls an archive member only to resolve a symbol that is still undefined), so
/// a target that does link the library gets no duplicate symbols. The natives and the
/// C++ standard library are repeated after the archives because `--as-needed` linkers
/// only keep a shared library for the references that come before it.
fn link_args(archives: &[&Archive], natives: &[NativeLib], stdlib: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = archives
        .iter()
        .map(|a| a.dir.join(format!("lib{}.a", a.name)).display().to_string())
        .collect();
    for n in natives {
        args.extend(n.dir.as_ref().map(|dir| format!("-L{}", dir.display())));
        args.push(format!("-l{}", n.name));
    }
    args.extend(stdlib.map(|l| format!("-l{l}")));
    args.into_iter()
        .map(|a| format!("cargo::rustc-link-arg={a}"))
        .collect()
}

/// The configured base build (the flags every translation unit shares) and the real
/// compiler (before any ccache wrapping), which identifies the toolchain in cache keys.
fn base_build(i: &Inputs<'_>, runtime: &Runtime) -> Result<(cc::Build, cc::Tool), Error> {
    let env = i.env;
    let mut b = cc::Build::new();
    b.cpp(true)
        .std(CXX_STD)
        .target(&env.target)
        .host(&env.host)
        .opt_level_str(&env.cxx_opt)
        .debug(env.debug)
        // simjig-build prints its own `cargo::` lines (see `Built::directives`).
        .cargo_metadata(false)
        // Warnings in generated code are not actionable for the user (`-w`).
        .warnings(false)
        .extra_warnings(false)
        .cargo_warnings(false)
        .cargo_debug(false)
        .flag("-include")
        .flag(runtime.config_header().display().to_string())
        .include(runtime.dir())
        .include(&i.verilator.include_dir)
        .include(i.verilator.include_dir.join("vltstd"))
        .includes(&i.recipe.include_dirs)
        .flags(&i.recipe.flags);
    let compiler = b.try_get_compiler().map_err(|source| Error::Compile {
        what: "the model (finding the C++ compiler)".to_owned(),
        source,
    })?;
    if let Some(wrapper) = ccache_compiler(env, &compiler)? {
        b.compiler(wrapper);
    }
    Ok((b, compiler))
}

/// A native library a runtime extra links against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeLib {
    name: &'static str,
    /// Where it was found; `None` leaves the search to the linker's default paths.
    dir: Option<PathBuf>,
}

impl NativeLib {
    /// `<prefix>/include` next to `<prefix>/lib`, for the library's headers.
    fn include_dir(&self) -> Option<PathBuf> {
        let inc = self.dir.as_ref()?.parent()?.join("include");
        inc.is_dir().then_some(inc)
    }

    /// The link directives. A library found outside the default paths also gets an
    /// rpath: linkers prefer the shared library when both kinds are there (conda ships
    /// lz4 only as a shared library), and the test binary must find it at run time.
    /// `rustc-link-arg` reaches only the binaries and tests of the package whose build
    /// script calls simjig-build, which is where simjig models are linked.
    fn directives(&self) -> Vec<String> {
        let mut d = Vec::new();
        if let Some(dir) = &self.dir {
            d.push(format!("cargo::rustc-link-search=native={}", dir.display()));
        }
        d.push(format!("cargo::rustc-link-lib={}", self.name));
        if let Some(dir) = &self.dir {
            d.push(format!(
                "cargo::rustc-link-arg=-Wl,-rpath,{}",
                dir.display()
            ));
        }
        d
    }
}

/// The native libraries the model's runtime extras need: `verilated.mk` adds
/// `-llz4 -lz` for FST (5.052's `fstcpp` writer compresses with both).
fn native_libs(env: &BuildEnv, mk: &ClassesMk) -> Vec<NativeLib> {
    if !mk.on("VM_TRACE_FST") {
        return Vec::new();
    }
    ["lz4", "z"]
        .into_iter()
        .map(|name| find_native(env, name))
        .collect()
}

fn find_native(env: &BuildEnv, name: &'static str) -> NativeLib {
    let files = [
        format!("lib{name}.dylib"),
        format!("lib{name}.so"),
        format!("lib{name}.a"),
    ];
    let dir = env
        .lib_dirs()
        .into_iter()
        .find(|d| files.iter().any(|f| d.join(f).is_file()));
    NativeLib { name, dir }
}

/// The C++ standard library to link, following the `cc` crate's rules: `CXXSTDLIB` when
/// set (empty means none), otherwise libc++ on Apple and the BSDs and libstdc++ elsewhere.
fn cxx_stdlib(env: &BuildEnv) -> Option<String> {
    if let Some(lib) = &env.cxxstdlib {
        let lib = lib.to_string_lossy();
        return (!lib.is_empty()).then(|| lib.into_owned());
    }
    let t = &env.target;
    if t.contains("msvc") {
        None
    } else if t.contains("apple") || t.contains("freebsd") || t.contains("openbsd") {
        Some("c++".to_owned())
    } else if t.contains("android") {
        Some("c++_shared".to_owned())
    } else {
        Some("stdc++".to_owned())
    }
}

/// When ccache is available and the compiler is not already wrapped, returns a tiny
/// shell script that runs the compiler through it.
///
/// Why a script: `cc` only recognises a wrapper inside the `CXX` variable of the process
/// environment, which a build script cannot change, and `cc::Build::compiler` takes a
/// single executable. The script keeps `cc`'s compiler detection working (it runs the
/// real compiler) while every compile goes through ccache. Skipped when `CXX` carries
/// its own arguments or wrapper, when `SIMJIG_CCACHE=0`, and on non-Unix hosts.
fn ccache_compiler(env: &BuildEnv, tool: &cc::Tool) -> Result<Option<PathBuf>, Error> {
    if !cfg!(unix)
        || env
            .cxx
            .as_ref()
            .is_some_and(|c| c.to_string_lossy().contains(' '))
    {
        return Ok(None);
    }
    let ccache = match &env.simjig_ccache {
        Some(v) if v == "0" => return Ok(None),
        Some(v) => Some(PathBuf::from(v)),
        None => env.find_on_path("ccache"),
    };
    let Some(ccache) = ccache else {
        return Ok(None);
    };
    if !tool.cc_env().is_empty() {
        return Ok(None);
    }
    std::fs::create_dir_all(&env.out_dir).map_err(Error::io("creating", &env.out_dir))?;
    let script = env.out_dir.join("simjig-cxx-ccache");
    let text = format!(
        "#!/bin/sh\n# @generated by simjig-build: run the C++ compiler through ccache.\n\
         exec {} {} \"$@\"\n",
        sh_quote(&ccache),
        sh_quote(tool.path())
    );
    std::fs::write(&script, text).map_err(Error::io("writing", &script))?;
    make_executable(&script)?;
    Ok(Some(script))
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

fn make_executable(p: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(p, perms).map_err(Error::io("making executable", p))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape of a real Verilator 5.052 `_classes.mk` (with --trace-fst --coverage).
    const MK: &str = "# Verilated -*- Makefile -*-\n### Switches...\n# C11?\nVM_C11 = 1\n\
        VM_TIMING = 0\nVM_COVERAGE = 1\nVM_PARALLEL_BUILDS = 0\nVM_TRACE = 1\n\
        VM_TRACE_FST = 1\nVM_TRACE_SAIF = 0\nVM_TRACE_VCD = 0\nVM_VPI = 0\n\n\
        ### Object file lists...\n# fast\nVM_CLASSES_FAST += \\\n  Vx \\\n  Vx___024root__0 \\\n\n\
        # slow\nVM_CLASSES_SLOW += \\\n  Vx___024root__Slow \\\n\n\
        VM_SUPPORT_FAST += \\\n  Vx__Trace__0 \\\n\nVM_SUPPORT_SLOW += \\\n  Vx__Syms__Slow \\\n\n\
        # Global classes\nVM_GLOBAL_FAST += \\\n  verilated \\\n  verilated_cov \\\n  verilated_fst_c \\\n\n\
        VM_GLOBAL_SLOW += \\\n\n\n# Verilated -*- Makefile -*-\n";

    #[test]
    fn classes_mk_lists_and_switches() {
        let mk = ClassesMk::parse(Path::new("Vx_classes.mk"), MK).unwrap();
        assert_eq!(
            mk.model,
            [
                "Vx",
                "Vx___024root__0",
                "Vx___024root__Slow",
                "Vx__Trace__0",
                "Vx__Syms__Slow"
            ]
        );
        assert_eq!(mk.global, ["verilated", "verilated_cov", "verilated_fst_c"]);
        assert!(mk.on("VM_TRACE_FST") && mk.on("VM_COVERAGE") && !mk.on("VM_TIMING"));
        let defines: Vec<String> = mk
            .defines()
            .into_iter()
            .map(|(k, v)| format!("{k}={}", v.unwrap_or_default()))
            .collect();
        assert_eq!(
            defines.join(" "),
            "VERILATOR=1 VM_SC=0 VM_COVERAGE=1 VM_TIMING=0 VM_TRACE=1 VM_TRACE_FST=1 \
             VM_TRACE_VCD=0 VM_TRACE_SAIF=0 VM_VPI=0"
        );
    }

    #[test]
    fn extras_are_the_runtime_classes_beyond_the_base() {
        let mk = ClassesMk::parse(Path::new("Vx_classes.mk"), MK).unwrap();
        assert_eq!(
            mk.extras().collect::<Vec<_>>(),
            ["verilated_cov", "verilated_fst_c"]
        );
    }

    #[test]
    fn fst_needs_lz4_and_zlib_found_on_the_lib_dirs() {
        let t = tempfile::tempdir().unwrap();
        let lib = t.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(t.path().join("include")).unwrap();
        std::fs::write(lib.join("liblz4.dylib"), "").unwrap();
        let env = BuildEnv {
            out_dir: t.path().into(),
            target: "aarch64-apple-darwin".into(),
            host: String::new(),
            cxx_opt: "2".into(),
            debug: false,
            path: None,
            verilator_root: None,
            untested_verilator: false,
            simjig_verilator: None,
            simjig_cache_dir: None,
            simjig_ccache: None,
            cxx: None,
            cxxstdlib: None,
            cargo_target_dir: None,
            conda_prefix: Some(t.path().into()),
            ldflags: None,
        };
        let mk = ClassesMk::parse(Path::new("Vx_classes.mk"), MK).unwrap();
        let libs = native_libs(&env, &mk);
        assert_eq!(
            libs[0].directives(),
            [
                format!("cargo::rustc-link-search=native={}", lib.display()),
                "cargo::rustc-link-lib=lz4".to_owned(),
                format!("cargo::rustc-link-arg=-Wl,-rpath,{}", lib.display()),
            ]
        );
        assert_eq!(libs[0].include_dir(), Some(t.path().join("include")));
        // Not found: left to the linker's default paths.
        assert_eq!(libs[1].directives(), ["cargo::rustc-link-lib=z"]);
        let plain = MK.replace("VM_TRACE_FST = 1", "VM_TRACE_FST = 0");
        let mk = ClassesMk::parse(Path::new("Vx_classes.mk"), &plain).unwrap();
        assert_eq!(native_libs(&env, &mk), []);
    }

    #[test]
    fn classes_mk_without_switches_is_rejected() {
        let err = ClassesMk::parse(
            Path::new("V_classes.mk"),
            "VM_CLASSES_FAST += \\\n  V \\\n\n",
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "V_classes.mk:0: missing switch `VM_COVERAGE`"
        );
    }
}
