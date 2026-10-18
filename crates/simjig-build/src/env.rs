//! The build-script environment, captured once.
//!
//! Everything `simjig-build` reads from the environment goes through [`BuildEnv`], so the
//! whole pipeline can run outside Cargo (tests, tools) with an explicit environment, and
//! so the list of variables that need `cargo::rerun-if-env-changed` is in one place
//! ([`BuildEnv::TRACKED_VARS`]).
//!
//! Exception: the `cc` crate reads `CXX`, `CXXFLAGS` and friends from the process
//! environment itself; they are tracked here, and `CXX` is captured only to decide about
//! ccache.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Error;

/// Inputs from the environment of one build.
#[derive(Debug, Clone)]
pub struct BuildEnv {
    /// Cargo's `OUT_DIR`: per-crate output (compiled model library, generated files).
    pub out_dir: PathBuf,
    /// Target triple (`TARGET`).
    pub target: String,
    /// Host triple (`HOST`).
    pub host: String,
    /// Optimisation level for all C++ (`-O<cxx_opt>`): `SIMJIG_CXX_OPT` when set, else
    /// [`BuildEnv::DEFAULT_CXX_OPT`]. Deliberately *not* Cargo's `OPT_LEVEL`: a dev build
    /// compiles Verilator output at `-O0`, which makes models unusably slow, and model
    /// code is not what a testbench author debugs. `CXXFLAGS` from the environment come
    /// after it on the compiler command line (the `cc` crate's order), so an `-O` there
    /// still wins.
    pub cxx_opt: String,
    /// Whether to emit debug info in the C++ (`DEBUG`).
    pub debug: bool,
    /// `PATH`, searched for `verilator` and `ccache`.
    pub path: Option<OsString>,
    /// `VERILATOR_ROOT`: the build runs `$VERILATOR_ROOT/bin/verilator` and compiles
    /// `$VERILATOR_ROOT/include`.
    ///
    /// With a packaged Verilator (conda-forge, as pixi installs it) the root is
    /// `<prefix>/share/verilator`, and its `bin/verilator` is a wrapper that runs
    /// `<prefix>/bin/verilator`, found by relative path, which in turn refuses any
    /// `VERILATOR_ROOT` but its own `../share/verilator`. A root copied elsewhere must
    /// therefore be a copy of the whole prefix (at least `bin/verilator`,
    /// `bin/verilator_bin` and `share/verilator/`), with
    /// `VERILATOR_ROOT=<copy>/share/verilator`: a copy of `share/verilator` alone fails
    /// with `Exec failed`, and the original executable with a copied root with
    /// `VERILATOR_ROOT is set to inconsistent path` (checked on 5.052 from conda-forge).
    pub verilator_root: Option<PathBuf>,
    /// `SIMJIG_VERILATOR`: an explicit `verilator` executable; overrides the search.
    pub simjig_verilator: Option<PathBuf>,
    /// `SIMJIG_UNTESTED_VERILATOR=1`: accept a Verilator release outside the supported
    /// table, with a warning (`svtypes::toolchain::judge`).
    pub untested_verilator: bool,
    /// `SIMJIG_CACHE_DIR`: an explicit cache directory; overrides the default under the
    /// Cargo target directory.
    pub simjig_cache_dir: Option<PathBuf>,
    /// `SIMJIG_CCACHE`: `0` disables ccache, a path selects the executable; unset means
    /// "use `ccache` from PATH if there is one".
    pub simjig_ccache: Option<OsString>,
    /// `CXX`, consulted only to decide whether to add ccache (the `cc` crate reads it
    /// itself).
    pub cxx: Option<OsString>,
    /// `CXXSTDLIB`: the C++ standard library to link; empty means none.
    pub cxxstdlib: Option<OsString>,
    /// `CARGO_TARGET_DIR`, used for the shared cache when it is an absolute path.
    pub cargo_target_dir: Option<PathBuf>,
    /// `CONDA_PREFIX` (set by `pixi run`): its `lib/` is searched for the FST tracing
    /// libraries (lz4, zlib).
    pub conda_prefix: Option<PathBuf>,
    /// `LDFLAGS`: its `-L` directories are searched for the FST tracing libraries.
    pub ldflags: Option<OsString>,
}

impl BuildEnv {
    /// Variables whose change must rerun the build script.
    ///
    /// `PATH` is deliberately not among them, although `verilator` and `ccache` are
    /// looked up on it: the build script instead reruns when the *resolved* Verilator
    /// executable changes (`rerun-if-changed` on its path), so a shell whose `PATH`
    /// differs but resolves to the same Verilator (for example running a test outside
    /// `pixi run`) does not rerun and fail. A `PATH` change that resolves to a different
    /// Verilator is not noticed until something else reruns the script; select a
    /// Verilator with `SIMJIG_VERILATOR` or `VERILATOR_ROOT` (both tracked) instead.
    /// ccache does not change the output, and the `cc` crate does not track `PATH` either.
    pub const TRACKED_VARS: &'static [&'static str] = &[
        "VERILATOR_ROOT",
        "SIMJIG_VERILATOR",
        "SIMJIG_UNTESTED_VERILATOR",
        "SIMJIG_CACHE_DIR",
        "SIMJIG_CCACHE",
        "SIMJIG_CXX_OPT",
        "CARGO_TARGET_DIR",
        "CONDA_PREFIX",
        "LDFLAGS",
        "CXX",
        "CXXFLAGS",
        "CXXSTDLIB",
    ];

    /// The C++ optimisation level when `SIMJIG_CXX_OPT` is not set: Verilator's own
    /// recommendation for model code.
    pub const DEFAULT_CXX_OPT: &'static str = "2";

    /// Reads the environment Cargo gives a build script.
    pub fn from_cargo() -> Result<Self, Error> {
        let req =
            |var: &'static str| std::env::var(var).map_err(|_unset| Error::MissingEnv { var });
        let opt = |var: &str| std::env::var_os(var).filter(|v| !v.is_empty());
        Ok(Self {
            out_dir: PathBuf::from(req("OUT_DIR")?),
            target: req("TARGET")?,
            host: req("HOST")?,
            cxx_opt: cxx_opt(opt("SIMJIG_CXX_OPT"))?,
            debug: req("DEBUG")? != "false",
            path: opt("PATH"),
            verilator_root: opt("VERILATOR_ROOT").map(PathBuf::from),
            simjig_verilator: opt("SIMJIG_VERILATOR").map(PathBuf::from),
            untested_verilator: svtypes::toolchain::allows_untested(
                std::env::var_os(svtypes::toolchain::UNTESTED_VAR).as_deref(),
            ),
            simjig_cache_dir: opt("SIMJIG_CACHE_DIR").map(PathBuf::from),
            simjig_ccache: opt("SIMJIG_CCACHE"),
            cxx: opt("CXX"),
            cxxstdlib: std::env::var_os("CXXSTDLIB"),
            cargo_target_dir: opt("CARGO_TARGET_DIR").map(PathBuf::from),
            conda_prefix: opt("CONDA_PREFIX").map(PathBuf::from),
            ldflags: opt("LDFLAGS"),
        })
    }

    /// Directories to search for native libraries the model needs beyond the C++
    /// standard library: the `-L` directories of `LDFLAGS`, then `$CONDA_PREFIX/lib`.
    pub(crate) fn lib_dirs(&self) -> Vec<PathBuf> {
        let ldflags = self
            .ldflags
            .as_ref()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut dirs: Vec<PathBuf> = ldflags
            .split_whitespace()
            .filter_map(|f| f.strip_prefix("-L"))
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .collect();
        dirs.extend(self.conda_prefix.as_ref().map(|p| p.join("lib")));
        dirs
    }

    /// The shared cache directory, in this order: `SIMJIG_CACHE_DIR`;
    /// `$CARGO_TARGET_DIR/simjig-cache` when that is absolute; the target directory found
    /// above `OUT_DIR` (the nearest ancestor holding Cargo's `CACHEDIR.TAG`); and as a last
    /// resort `OUT_DIR/simjig-cache`, which is correct but not shared.
    pub fn cache_dir(&self) -> PathBuf {
        if let Some(dir) = &self.simjig_cache_dir {
            return dir.clone();
        }
        if let Some(dir) = self.cargo_target_dir.as_ref().filter(|d| d.is_absolute()) {
            return dir.join("simjig-cache");
        }
        target_dir_above(&self.out_dir)
            .unwrap_or(&self.out_dir)
            .join("simjig-cache")
    }

    /// Finds an executable named `name` on `PATH`.
    pub(crate) fn find_on_path(&self, name: &str) -> Option<PathBuf> {
        self.path_dirs()
            .into_iter()
            .map(|d| d.join(name))
            .find(|p| svtypes::toolchain::is_executable(p))
    }

    pub(crate) fn path_dirs(&self) -> Vec<PathBuf> {
        self.path
            .as_ref()
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default()
    }
}

/// Validates `SIMJIG_CXX_OPT`: the levels both GCC and Clang accept.
fn cxx_opt(value: Option<OsString>) -> Result<String, Error> {
    let Some(value) = value else {
        return Ok(BuildEnv::DEFAULT_CXX_OPT.to_owned());
    };
    let v = value.to_string_lossy();
    if ["0", "1", "2", "3", "s", "z", "g"].contains(&v.as_ref()) {
        Ok(v.into_owned())
    } else {
        Err(Error::BadEnv {
            var: "SIMJIG_CXX_OPT",
            value: v.into_owned(),
            expected: "one of 0, 1, 2, 3, s, z, g (the `-O` level for model C++)",
        })
    }
}

/// Cargo writes `CACHEDIR.TAG` at the root of every target (and build) directory.
fn target_dir_above(out_dir: &Path) -> Option<&Path> {
    out_dir
        .ancestors()
        .skip(1)
        .find(|d| d.join("CACHEDIR.TAG").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(out_dir: &Path) -> BuildEnv {
        BuildEnv {
            out_dir: out_dir.to_path_buf(),
            target: String::new(),
            host: String::new(),
            cxx_opt: "2".into(),
            debug: false,
            path: None,
            verilator_root: None,
            simjig_verilator: None,
            untested_verilator: false,
            simjig_cache_dir: None,
            simjig_ccache: None,
            cxx: None,
            cxxstdlib: None,
            cargo_target_dir: None,
            conda_prefix: None,
            ldflags: None,
        }
    }

    #[test]
    fn cxx_opt_defaults_to_o2_and_validates_overrides() {
        assert_eq!(cxx_opt(None).unwrap(), "2");
        assert_eq!(cxx_opt(Some("0".into())).unwrap(), "0");
        assert_eq!(cxx_opt(Some("s".into())).unwrap(), "s");
        assert_eq!(
            cxx_opt(Some("-O3".into())).unwrap_err().to_string(),
            "environment variable `SIMJIG_CXX_OPT` is `-O3`; expected one of 0, 1, 2, 3, s, z, g \
             (the `-O` level for model C++)"
        );
    }

    #[test]
    fn explicit_vars_are_tracked_but_path_is_not() {
        for var in [
            "SIMJIG_VERILATOR",
            "VERILATOR_ROOT",
            "SIMJIG_CXX_OPT",
            "CONDA_PREFIX",
            "LDFLAGS",
        ] {
            assert!(BuildEnv::TRACKED_VARS.contains(&var), "{var}");
        }
        assert!(!BuildEnv::TRACKED_VARS.contains(&"PATH"));
    }

    #[test]
    fn lib_dirs_come_from_ldflags_then_conda() {
        let mut e = env(Path::new("/o"));
        e.ldflags = Some("-Wl,-rpath,/x -L/a/lib -O1 -L/b".into());
        e.conda_prefix = Some(PathBuf::from("/conda"));
        assert_eq!(
            e.lib_dirs(),
            [
                PathBuf::from("/a/lib"),
                PathBuf::from("/b"),
                PathBuf::from("/conda/lib")
            ]
        );
    }

    #[test]
    fn cache_dir_is_derived_from_out_dir() {
        let t = tempfile::tempdir().unwrap();
        let target = t.path().join("target");
        let out = target.join("debug/build/foo-0123/out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature: x").unwrap();
        let mut e = env(&out);
        assert_eq!(e.cache_dir(), target.join("simjig-cache"));

        // A cross-compiling layout (`target/<triple>/debug/...`) resolves to the same root.
        e.out_dir = target.join("aarch64-apple-darwin/debug/build/foo-1/out");
        assert_eq!(e.cache_dir(), target.join("simjig-cache"));

        e.cargo_target_dir = Some(PathBuf::from("relative/is/ignored"));
        assert_eq!(e.cache_dir(), target.join("simjig-cache"));
        e.cargo_target_dir = Some(t.path().join("ct"));
        assert_eq!(e.cache_dir(), t.path().join("ct/simjig-cache"));
        e.simjig_cache_dir = Some(t.path().join("explicit"));
        assert_eq!(e.cache_dir(), t.path().join("explicit"));
    }

    #[test]
    fn cache_dir_falls_back_to_out_dir() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(env(t.path()).cache_dir(), t.path().join("simjig-cache"));
    }
}
