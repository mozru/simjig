//! Shared helpers: a build environment in a temp dir and a Verilator that logs its calls.

use std::path::{Path, PathBuf};
use std::process::Command;

use simjig_build::BuildEnv;

pub(crate) fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The real `verilator` on PATH (the pixi one).
pub(crate) fn real_verilator() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("verilator"))
        .find(|p| p.is_file())
        .expect("verilator must be on PATH: run the tests with `pixi run cargo nextest run`")
}

fn host_triple() -> String {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("run rustc -vV");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .expect("rustc -vV prints the host")
        .to_owned()
}

/// A temp dir with `out/` (`OUT_DIR`), `cache/` and a logging Verilator wrapper.
pub(crate) struct Sandbox {
    pub(crate) dir: tempfile::TempDir,
}

impl Sandbox {
    pub(crate) fn new() -> Self {
        let s = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(s.path().join("out")).unwrap();
        write_script(
            &s.wrapper(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
                s.log().display(),
                real_verilator().display()
            ),
        );
        s
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn wrapper(&self) -> PathBuf {
        self.path().join("verilator-wrapper")
    }

    fn log(&self) -> PathBuf {
        self.path().join("verilator.log")
    }

    /// How many times Verilator was asked to verilate (`--cc`), as opposed to the
    /// `--version`/`--getenv` queries every build makes.
    pub(crate) fn verilations(&self) -> usize {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .filter(|l| l.split(' ').any(|a| a == "--cc"))
            .count()
    }

    /// A build environment like Cargo's for a host build.
    pub(crate) fn env(&self) -> BuildEnv {
        BuildEnv {
            out_dir: self.path().join("out"),
            target: host_triple(),
            host: host_triple(),
            cxx_opt: "2".into(),
            debug: false,
            path: std::env::var_os("PATH"),
            verilator_root: None,
            simjig_verilator: Some(self.wrapper()),
            simjig_cache_dir: Some(self.path().join("cache")),
            simjig_ccache: None,
            cxx: std::env::var_os("CXX"),
            cxxstdlib: None,
            cargo_target_dir: None,
            conda_prefix: std::env::var_os("CONDA_PREFIX").map(PathBuf::from),
            ldflags: std::env::var_os("LDFLAGS"),
        }
    }

    /// Copies the fixture RTL into the sandbox, so a test may edit it.
    pub(crate) fn rtl(&self) -> PathBuf {
        let dst = self.path().join("rtl");
        copy_dir(&fixtures().join("rtl"), &dst);
        dst
    }
}

pub(crate) fn write_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub(crate) fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

/// `tests/duts/<name>` in the repository.
pub(crate) fn dut(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/duts")
        .join(name)
}

/// A stand-in for ccache that logs every compiler command line and runs it.
pub(crate) fn logging_ccache(s: &Sandbox) -> (PathBuf, PathBuf) {
    let script = s.path().join("fake-ccache");
    let log = s.path().join("cxx.log");
    write_script(
        &script,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec \"$@\"\n",
            log.display()
        ),
    );
    (script, log)
}
