//! Probing the machine: the only part of the doctor that runs commands or reads the
//! environment.
//!
//! Every Verilator probe runs with `VERILATOR_ROOT` removed. Verilator 5.052's launcher
//! script exits with an error on *any* command, `--version` included, when
//! `VERILATOR_ROOT` disagrees with the binary's own root; removing it lets the version
//! check and the root check report separately.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What running one probe command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Probe {
    /// The program is not installed.
    NotFound,
    /// The program started but failed; holds the first line of its error output.
    Failed(String),
    /// The program succeeded; holds its trimmed output.
    Output(String),
}

impl Probe {
    /// The first line of a successful probe's output.
    pub(super) fn first_line(&self) -> Option<&str> {
        match self {
            Self::Output(text) => Some(text.lines().next().unwrap_or("")),
            Self::NotFound | Self::Failed(_) => None,
        }
    }
}

/// `VERILATOR_ROOT` as set in the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EnvRoot {
    pub(super) raw: String,
    /// The canonical path, or `None` when it does not exist.
    pub(super) canonical: Option<PathBuf>,
}

/// Everything the checks need to know about the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Facts {
    pub(super) cwd: PathBuf,
    /// The `verilator` program found on `PATH`.
    pub(super) verilator: Option<PathBuf>,
    /// `verilator --version`.
    pub(super) verilator_version: Probe,
    /// `verilator --getenv VERILATOR_ROOT`: the root the binary itself uses.
    pub(super) verilator_root: Probe,
    pub(super) env_root: Option<EnvRoot>,
    /// `$CXX`, if set.
    pub(super) cxx: Option<String>,
    /// `$CXX --version`, or `c++ --version` when `CXX` is unset.
    pub(super) cxx_version: Probe,
    /// `ccache --version`.
    pub(super) ccache: Probe,
    /// `cargo nextest --version`.
    pub(super) nextest: Probe,
    /// The `make` program found on `PATH`.
    pub(super) make: Option<PathBuf>,
    /// `$PIXI_ENVIRONMENT_NAME`, set by `pixi run` and `pixi shell`.
    pub(super) pixi_environment: Option<String>,
    /// `$CONDA_PREFIX`, the pixi environment's install prefix.
    pub(super) conda_prefix: Option<String>,
    /// `SIMJIG_UNTESTED_VERILATOR=1`: accept an unsupported Verilator with a warning.
    pub(super) allow_untested: bool,
}

/// The compiler the `cc` crate falls back to when `CXX` is unset.
pub(super) const DEFAULT_CXX: &str = "c++";

impl Facts {
    pub(super) fn gather() -> Self {
        let verilator = find_on_path("verilator");
        let verilator_probe = |args: &[&str]| match &verilator {
            Some(path) => probe(Command::new(path).args(args).env_remove("VERILATOR_ROOT")),
            None => Probe::NotFound,
        };
        let cxx = env_string("CXX");
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        Self {
            cwd: env::current_dir().unwrap_or_default(),
            verilator_version: verilator_probe(&["--version"]),
            verilator_root: verilator_probe(&["--getenv", "VERILATOR_ROOT"]),
            verilator,
            env_root: env_string("VERILATOR_ROOT").map(|raw| EnvRoot {
                canonical: Path::new(&raw).canonicalize().ok(),
                raw,
            }),
            cxx_version: probe_cxx(cxx.as_deref().unwrap_or(DEFAULT_CXX)),
            cxx,
            ccache: probe(Command::new("ccache").arg("--version")),
            nextest: probe(Command::new(cargo).args(["nextest", "--version"])),
            make: find_on_path("make"),
            pixi_environment: env_string("PIXI_ENVIRONMENT_NAME"),
            conda_prefix: env_string("CONDA_PREFIX"),
            allow_untested: env::var_os("SIMJIG_UNTESTED_VERILATOR").is_some_and(|v| v == "1"),
        }
    }
}

/// A set, non-empty environment variable.
fn env_string(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// Runs `$CXX --version`. `CXX` may hold a launcher and arguments (`ccache clang++`).
fn probe_cxx(cxx: &str) -> Probe {
    let mut words = cxx.split_whitespace();
    let Some(program) = words.next() else {
        return Probe::NotFound;
    };
    probe(Command::new(program).args(words).arg("--version"))
}

fn probe(command: &mut Command) -> Probe {
    match command.output() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Probe::NotFound,
        Err(error) => Probe::Failed(error.to_string()),
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success() {
                // Some tools print their version on stderr.
                let text = if stdout.trim().is_empty() {
                    stderr
                } else {
                    stdout
                };
                Probe::Output(text.trim().to_owned())
            } else {
                let line = stderr.lines().find(|l| !l.trim().is_empty());
                Probe::Failed(line.map_or_else(|| output.status.to_string(), str::to_owned))
            }
        }
    }
}

/// The first executable named `program` on `PATH`, like `command -v`.
fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file() || path.with_extension("exe").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn probe_classifies_outcomes() {
        assert_eq!(
            probe(&mut Command::new("simjig-doctor-no-such-program")),
            Probe::NotFound
        );
        assert_eq!(
            probe(Command::new("sh").args(["-c", "echo ' v1.2 '"])),
            Probe::Output("v1.2".to_owned())
        );
        assert_eq!(
            probe(Command::new("sh").args(["-c", "echo v3 >&2"])),
            Probe::Output("v3".to_owned())
        );
        assert_eq!(
            probe(Command::new("sh").args(["-c", "echo; echo bad >&2; echo worse >&2; exit 3"])),
            Probe::Failed("bad".to_owned())
        );
    }
}
