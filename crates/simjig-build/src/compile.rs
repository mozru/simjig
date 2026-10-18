//! Compiling Verilator's C++ output into static libraries and linking them.
//!
//! Two libraries per model:
//! - **libverilated** (Verilator's runtime, plus [`CppRecipe::global_sources`]) is built
//!   once per (Verilator version, defines, sources, compiler, target, optimisation) into
//!   the shared cache, because it is the same for every model with those settings and is
//!   the slowest part to compile.
//! - **the model** (the generated classes, plus [`CppRecipe::model_sources`]) is built
//!   into `OUT_DIR`.
//!
//! The file lists come from Verilator's own `<prefix>_classes.mk`, never from guessing,
//! and the `VM_*` defines from the switches in the same file, exactly as Verilator's
//! `verilated.mk` passes them. ccache is used when available (see [`ccache_compiler`]).

use std::path::{Path, PathBuf};

use crate::cache::{self, Cache};
use crate::hash::KeyHasher;
use crate::verilator::{self, Verilator};
use crate::{ABI_VERSION, BuildEnv, Error};

/// How the generated C++ is compiled: the seam where the C ABI runtime plugs in.
///
/// The default compiles exactly what Verilator's own makefiles would: libverilated and
/// the model's classes, with no extra defines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CppRecipe {
    /// Preprocessor defines for *every* translation unit, libverilated included (for
    /// example `VL_USER_FATAL`, which removes `vl_fatal` from `verilated.cpp`). A `None`
    /// value means `-DNAME` without a value.
    pub defines: Vec<(String, Option<String>)>,
    /// Sources linked once per executable, compiled into libverilated (for example the
    /// `VL_USER_*` hook implementations).
    pub global_sources: Vec<PathBuf>,
    /// Sources compiled with each model (for example the per-model vtable shim).
    pub model_sources: Vec<PathBuf>,
    /// Extra include directories for every translation unit.
    pub include_dirs: Vec<PathBuf>,
    /// Extra compiler flags for every translation unit.
    pub flags: Vec<String>,
}

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

    /// The `-D` flags `verilated.mk` passes to every translation unit, plus
    /// `VL_TIME_CONTEXT`: the testbench owns time and sets it on the `VerilatedContext`,
    /// so the runtime must not reference the legacy `sc_time_stamp()` hook. (That hook is
    /// a weak symbol, which Apple's linker rejects when undefined unless every final link
    /// passes `-Wl,-U,...` as `verilated.mk` does; build scripts cannot add link
    /// arguments to dependent crates.)
    fn defines(&self) -> Vec<(String, Option<String>)> {
        let mut d = vec![
            ("VERILATOR".to_owned(), Some("1".to_owned())),
            ("VM_SC".to_owned(), Some("0".to_owned())),
            ("VL_TIME_CONTEXT".to_owned(), None),
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
}

/// Compiles libverilated (cached) and the model, returning the link directives in the
/// order a static linker needs: the model before the runtime it calls.
pub(crate) fn compile(i: &Inputs<'_>) -> Result<Vec<String>, Error> {
    let mk_path = verilator::output_file(i.obj_dir, i.prefix, "_classes.mk");
    let text = std::fs::read_to_string(&mk_path).map_err(Error::io("reading", &mk_path))?;
    let mk = ClassesMk::parse(&mk_path, &text)?;
    let std = if mk.on("VM_TIMING") { "c++20" } else { "c++17" };
    let mut defines = mk.defines();
    defines.extend(i.recipe.defines.iter().cloned());
    let (base, compiler) = base_build(i, std, &defines)?;

    let (lib_dir, lib_name) = libverilated(i, &base, &compiler, &mk, &defines, std)?;

    let model_name = format!("simjig_model_{}", i.prefix);
    let out = i.env.out_dir.join(format!("simjig-{}", i.prefix));
    std::fs::create_dir_all(&out).map_err(Error::io("creating", &out))?;
    let mut b = base;
    b.out_dir(&out).include(i.obj_dir);
    b.files(mk.model.iter().map(|c| i.obj_dir.join(format!("{c}.cpp"))));
    b.files(&i.recipe.model_sources);
    b.try_compile(&model_name)
        .map_err(|source| Error::Compile {
            what: format!("the `{}` model", i.prefix),
            source,
        })?;

    let mut d = vec![
        format!("cargo::rustc-link-search=native={}", out.display()),
        format!("cargo::rustc-link-lib=static={model_name}"),
        format!("cargo::rustc-link-search=native={}", lib_dir.display()),
        format!("cargo::rustc-link-lib=static={lib_name}"),
    ];
    d.extend(cxx_stdlib(i.env).map(|l| format!("cargo::rustc-link-lib={l}")));
    if mk.on("VM_TRACE_FST") {
        // verilated.mk: `LDLIBS += -llz4 -lz` for FST.
        d.extend([
            "cargo::rustc-link-lib=lz4".to_owned(),
            "cargo::rustc-link-lib=z".to_owned(),
        ]);
    }
    Ok(d)
}

/// Returns the configured build and the real compiler (before any ccache wrapping), which
/// identifies the toolchain in the libverilated key.
fn base_build(
    i: &Inputs<'_>,
    std: &str,
    defines: &[(String, Option<String>)],
) -> Result<(cc::Build, cc::Tool), Error> {
    let env = i.env;
    let mut b = cc::Build::new();
    b.cpp(true)
        .std(std)
        .target(&env.target)
        .host(&env.host)
        .opt_level_str(&env.opt_level)
        .debug(env.debug)
        // simjig-build prints its own `cargo::` lines (see `Built::directives`).
        .cargo_metadata(false)
        // Warnings in generated code are not actionable for the user.
        .warnings(false)
        .extra_warnings(false)
        .cargo_warnings(false)
        .include(&i.verilator.include_dir)
        .include(i.verilator.include_dir.join("vltstd"))
        .includes(&i.recipe.include_dirs)
        .flags(&i.recipe.flags);
    for (k, v) in defines {
        b.define(k, v.as_deref());
    }
    let compiler = b.try_get_compiler().map_err(|source| Error::Compile {
        what: "the model (finding the C++ compiler)".to_owned(),
        source,
    })?;
    if let Some(wrapper) = ccache_compiler(env, &compiler)? {
        b.compiler(wrapper);
    }
    Ok((b, compiler))
}

/// Builds libverilated into the cache unless it is there; returns (dir, link name).
fn libverilated(
    i: &Inputs<'_>,
    base: &cc::Build,
    compiler: &cc::Tool,
    mk: &ClassesMk,
    defines: &[(String, Option<String>)],
    std: &str,
) -> Result<(PathBuf, String), Error> {
    let mut h = KeyHasher::new("simjig-libverilated-v1");
    // The simjig-build version covers the fixed flags set in `base_build`.
    h.str(&ABI_VERSION.to_string())
        .str(env!("CARGO_PKG_VERSION"))
        .str(&i.verilator.version_line)
        .path(&i.verilator.include_dir)
        .path(compiler.path())
        .str(&i.env.target)
        .str(&i.env.opt_level)
        .field(&[u8::from(i.env.debug)])
        .str(std);
    for c in &mk.global {
        h.str(c);
    }
    for (k, v) in defines {
        h.str(k).str(v.as_deref().unwrap_or("\0none"));
    }
    for s in &i.recipe.global_sources {
        h.file(s)?;
    }
    for d in &i.recipe.include_dirs {
        h.path(d);
    }
    for f in &i.recipe.flags {
        h.str(f);
    }
    let key = h.finish();
    let name = format!("simjig_verilated_{}", &key[..16]);
    let dir = i.cache.root().join("libverilated").join(&key);
    if dir.join(format!("lib{name}.a")).is_file() {
        return Ok((dir, name));
    }
    let work = i.cache.work_dir()?;
    let mut b = base.clone();
    b.out_dir(&work);
    b.files(
        mk.global
            .iter()
            .map(|c| i.verilator.include_dir.join(format!("{c}.cpp"))),
    );
    b.files(&i.recipe.global_sources);
    if let Err(source) = b.try_compile(&name) {
        cache::remove_best_effort(&work);
        return Err(Error::Compile {
            what: "libverilated".to_owned(),
            source,
        });
    }
    Cache::publish(&work, &dir)?;
    Ok((dir, name))
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
            "VERILATOR=1 VM_SC=0 VL_TIME_CONTEXT= VM_COVERAGE=1 VM_TIMING=0 VM_TRACE=1 VM_TRACE_FST=1 \
             VM_TRACE_VCD=0 VM_TRACE_SAIF=0 VM_VPI=0"
        );
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
