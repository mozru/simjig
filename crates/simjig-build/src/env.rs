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
    /// Optimisation level for the C++ (`OPT_LEVEL`: `0`-`3`, `s` or `z`).
    pub opt_level: String,
    /// Whether to emit debug info in the C++ (`DEBUG`).
    pub debug: bool,
    /// `PATH`, searched for `verilator` and `ccache`.
    pub path: Option<OsString>,
    /// `VERILATOR_ROOT`.
    pub verilator_root: Option<PathBuf>,
    /// `SIMJIG_VERILATOR`: an explicit `verilator` executable; overrides the search.
    pub simjig_verilator: Option<PathBuf>,
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
}

impl BuildEnv {
    /// Variables whose change must rerun the build script.
    pub const TRACKED_VARS: &'static [&'static str] = &[
        "VERILATOR_ROOT",
        "SIMJIG_VERILATOR",
        "SIMJIG_CACHE_DIR",
        "SIMJIG_CCACHE",
        "CARGO_TARGET_DIR",
        "CXX",
        "CXXFLAGS",
        "CXXSTDLIB",
    ];

    /// Reads the environment Cargo gives a build script.
    pub fn from_cargo() -> Result<Self, Error> {
        let req =
            |var: &'static str| std::env::var(var).map_err(|_unset| Error::MissingEnv { var });
        let opt = |var: &str| std::env::var_os(var).filter(|v| !v.is_empty());
        Ok(Self {
            out_dir: PathBuf::from(req("OUT_DIR")?),
            target: req("TARGET")?,
            host: req("HOST")?,
            opt_level: req("OPT_LEVEL")?,
            debug: req("DEBUG")? != "false",
            path: opt("PATH"),
            verilator_root: opt("VERILATOR_ROOT").map(PathBuf::from),
            simjig_verilator: opt("SIMJIG_VERILATOR").map(PathBuf::from),
            simjig_cache_dir: opt("SIMJIG_CACHE_DIR").map(PathBuf::from),
            simjig_ccache: opt("SIMJIG_CCACHE"),
            cxx: opt("CXX"),
            cxxstdlib: std::env::var_os("CXXSTDLIB"),
            cargo_target_dir: opt("CARGO_TARGET_DIR").map(PathBuf::from),
        })
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
            .find(|p| is_executable(p))
    }

    pub(crate) fn path_dirs(&self) -> Vec<PathBuf> {
        self.path
            .as_ref()
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default()
    }
}

/// Cargo writes `CACHEDIR.TAG` at the root of every target (and build) directory.
fn target_dir_above(out_dir: &Path) -> Option<&Path> {
    out_dir
        .ancestors()
        .skip(1)
        .find(|d| d.join("CACHEDIR.TAG").is_file())
}

pub(crate) fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(out_dir: &Path) -> BuildEnv {
        BuildEnv {
            out_dir: out_dir.to_path_buf(),
            target: String::new(),
            host: String::new(),
            opt_level: "0".into(),
            debug: false,
            path: None,
            verilator_root: None,
            simjig_verilator: None,
            simjig_cache_dir: None,
            simjig_ccache: None,
            cxx: None,
            cxxstdlib: None,
            cargo_target_dir: None,
        }
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
