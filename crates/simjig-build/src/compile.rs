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
//!    and [`CppRecipe::model_sources`], into `OUT_DIR`. Its `VM_*` defines are the
//!    model's own switches from `_classes.mk`, exactly as `verilated.mk` passes them.
//! 2. **runtime extras**: runtime classes that only some models need (`verilated_fst_c`
//!    for `--trace-fst`, `verilated_cov` for `--coverage`), one cached archive per class,
//!    compiled with the switches of the model that asked for it.
//! 3. **the shared libverilated**: `verilated.cpp`, `verilated_threads.cpp`,
//!    `verilated_timing.cpp`, `simjig_rt.cpp` and [`CppRecipe::global_sources`], with
//!    fixed `VM_*` defines ([`RUNTIME_SWITCHES`]). Fixed, so that every model of a
//!    configuration links the *same* archive: two archives defining the same runtime
//!    symbols in one binary would be an ODR violation. That is sound because 5.052's
//!    runtime sources read only `VM_SC` and `VM_VPI` (and `VM_TRACE` for a compatibility
//!    macro), and generated code reads no `VM_*` at all (checked with grep, abi.md).
//!    Cached once per (Verilator version, compiler, target, flags, runtime sources);
//!    the runtime sources include `simjig_vl_config.h`.
//! 4. native libraries the extras need (`lz4`, `z` for FST), then the C++ standard
//!    library.
//!
//! ccache is used when available (see [`ccache_compiler`]).

use std::path::{Path, PathBuf};

use crate::cache::{self, Cache};
use crate::hash::KeyHasher;
use crate::runtime::Runtime;
use crate::verilator::{self, Verilator};
use crate::{ABI_VERSION, BuildEnv, Error};

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
    pub(crate) cache: &'a Cache,
    pub(crate) verilator: &'a Verilator,
    pub(crate) prefix: &'a str,
    pub(crate) obj_dir: &'a Path,
    pub(crate) recipe: &'a CppRecipe,
    /// Generated sources compiled with the model (the emitted shim).
    pub(crate) model_sources: &'a [PathBuf],
}

/// A static library, compiled or found in the cache.
struct Archive {
    dir: PathBuf,
    name: String,
}

/// Compiles the libraries (see the module docs) and returns the link directives in the
/// order a static linker needs: each library before the ones it calls.
pub(crate) fn compile(i: &Inputs<'_>) -> Result<Vec<String>, Error> {
    let mk_path = verilator::output_file(i.obj_dir, i.prefix, "_classes.mk");
    let text = std::fs::read_to_string(&mk_path).map_err(Error::io("reading", &mk_path))?;
    let mk = ClassesMk::parse(&mk_path, &text)?;
    let runtime = Runtime::extract(i.cache)?;
    let (base, compiler) = base_build(i, &runtime)?;
    let tc = Toolchain {
        i,
        base: &base,
        compiler: &compiler,
    };

    let mut model_defines = mk.defines();
    model_defines.extend(i.recipe.defines.iter().cloned());
    let model = compile_model(i, &base, &mk, &model_defines)?;

    let natives = native_libs(i.env, &mk);
    let mut extras = Vec::new();
    for class in mk.extras() {
        let src = i.verilator.include_dir.join(format!("{class}.cpp"));
        let includes: Vec<PathBuf> = natives.iter().filter_map(NativeLib::include_dir).collect();
        extras.push(tc.cached_archive(class, &[src], &model_defines, &includes)?);
    }

    let mut runtime_defines: Vec<(String, Option<String>)> = RUNTIME_SWITCHES
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Some((*v).to_owned())))
        .collect();
    runtime_defines.extend(i.recipe.defines.iter().cloned());
    let mut sources: Vec<PathBuf> = BASE_RUNTIME
        .iter()
        .map(|c| i.verilator.include_dir.join(format!("{c}.cpp")))
        .collect();
    sources.push(runtime.source());
    sources.extend(i.recipe.global_sources.iter().cloned());
    let shared = tc.cached_archive("verilated", &sources, &runtime_defines, &[])?;

    let mut d = Vec::new();
    for a in std::iter::once(&model).chain(&extras).chain([&shared]) {
        d.push(format!(
            "cargo::rustc-link-search=native={}",
            a.dir.display()
        ));
        d.push(format!("cargo::rustc-link-lib=static={}", a.name));
    }
    for n in &natives {
        d.extend(n.directives());
    }
    d.extend(cxx_stdlib(i.env).map(|l| format!("cargo::rustc-link-lib={l}")));
    Ok(d)
}

fn compile_model(
    i: &Inputs<'_>,
    base: &cc::Build,
    mk: &ClassesMk,
    defines: &[(String, Option<String>)],
) -> Result<Archive, Error> {
    let name = format!("simjig_model_{}", i.prefix);
    let dir = i.env.out_dir.join(format!("simjig-{}", i.prefix));
    std::fs::create_dir_all(&dir).map_err(Error::io("creating", &dir))?;
    let mut b = base.clone();
    b.out_dir(&dir).include(i.obj_dir);
    for (k, v) in defines {
        b.define(k, v.as_deref());
    }
    b.files(mk.model.iter().map(|c| i.obj_dir.join(format!("{c}.cpp"))));
    b.files(i.model_sources);
    b.files(&i.recipe.model_sources);
    b.try_compile(&name).map_err(|source| Error::Compile {
        what: format!("the `{}` model", i.prefix),
        source,
    })?;
    Ok(Archive { dir, name })
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

/// What identifies a runtime archive besides its own sources and defines.
struct Toolchain<'a> {
    i: &'a Inputs<'a>,
    base: &'a cc::Build,
    compiler: &'a cc::Tool,
}

impl Toolchain<'_> {
    /// Builds `simjig_<what>_<key>` from `sources` into the cache unless it is there.
    fn cached_archive(
        &self,
        what: &str,
        sources: &[PathBuf],
        defines: &[(String, Option<String>)],
        includes: &[PathBuf],
    ) -> Result<Archive, Error> {
        let i = self.i;
        let mut h = KeyHasher::new("simjig-libverilated-v2");
        // The simjig-build version covers the fixed flags set in `base_build`; the
        // runtime directory is content-addressed, so its path covers its headers.
        h.str(&ABI_VERSION.to_string())
            .str(env!("CARGO_PKG_VERSION"))
            .str(&i.verilator.version_line)
            .path(&i.verilator.include_dir)
            .path(self.compiler.path())
            .str(&i.env.target)
            .str(&i.env.cxx_opt)
            .field(&[u8::from(i.env.debug)])
            .str(what);
        for f in self.compiler.args() {
            h.field(f.as_encoded_bytes());
        }
        for (k, v) in defines {
            h.str(k).str(v.as_deref().unwrap_or("\0none"));
        }
        for s in sources {
            h.file(s)?;
        }
        for d in includes {
            h.path(d);
        }
        let key = h.finish();
        let name = format!("simjig_{what}_{}", &key[..16]);
        let dir = i.cache.root().join("libverilated").join(&key);
        if dir.join(format!("lib{name}.a")).is_file() {
            return Ok(Archive { dir, name });
        }
        let work = i.cache.work_dir()?;
        let mut b = self.base.clone();
        b.out_dir(&work).includes(includes).files(sources);
        for (k, v) in defines {
            b.define(k, v.as_deref());
        }
        if let Err(source) = b.try_compile(&name) {
            cache::remove_best_effort(&work);
            return Err(Error::Compile {
                what: format!("the Verilator runtime (`{what}`)"),
                source,
            });
        }
        Cache::publish(&work, &dir)?;
        Ok(Archive { dir, name })
    }
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
