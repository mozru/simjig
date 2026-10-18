//! Finding Verilator, checking its version, and running it.
//!
//! Where to look and which versions to accept are `svtypes::toolchain`'s rule, which
//! `cargo simjig doctor` applies too. The version gate exists because simjig decodes
//! Verilator's output and links against its runtime, whose formats change between
//! releases.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use svtypes::toolchain::{self, Search, Verdict};
use svtypes::version::VerilatorVersion;

use crate::{BuildEnv, Error};

/// A located, version-checked Verilator installation.
#[derive(Debug, Clone)]
pub struct Verilator {
    /// The `verilator` executable.
    pub exe: PathBuf,
    /// The parsed version.
    pub version: VerilatorVersion,
    /// The full first line of `verilator --version`; part of every cache key, so two
    /// builds of the same release from different sources never share outputs.
    pub version_line: String,
    /// `$VERILATOR_ROOT/include`, holding `verilated.cpp` and its headers.
    pub include_dir: PathBuf,
    /// `VERILATOR_ROOT` from the [`BuildEnv`], set for every Verilator run, so the
    /// executable finds the same installation whether or not the process has it.
    pub verilator_root: Option<PathBuf>,
    /// Not a supported release, accepted because `SIMJIG_UNTESTED_VERILATOR=1`; the
    /// build warns.
    pub untested: bool,
}

impl Verilator {
    /// Finds Verilator and checks its version by `svtypes::toolchain`'s rule
    /// (`SIMJIG_VERILATOR`, then `$VERILATOR_ROOT/bin/verilator`, then `PATH`; a
    /// supported release, or any release with `SIMJIG_UNTESTED_VERILATOR=1`), then asks
    /// it for its include directory.
    pub fn locate(env: &BuildEnv) -> Result<Self, Error> {
        let search = Search {
            simjig_verilator: env.simjig_verilator.clone(),
            verilator_root: env.verilator_root.clone(),
            path: env.path.clone(),
        };
        let exe = toolchain::locate(&search).map_err(|e| Error::VerilatorNotFound {
            searched: e.searched,
        })?;
        let mut found = Self {
            exe,
            version: VerilatorVersion::new(0, 0),
            version_line: String::new(),
            include_dir: PathBuf::new(),
            verilator_root: env.verilator_root.clone(),
            untested: false,
        };
        let out = run(found.command().arg("--version"))?;
        let version_line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let (version, untested) = match toolchain::judge(&version_line, env.untested_verilator) {
            Verdict::Supported(v) => (v, false),
            Verdict::Untested(v) => (v, true),
            Verdict::Refused(_) => {
                return Err(Error::UnsupportedVerilator {
                    path: found.exe.clone(),
                    found: version_line,
                });
            }
        };
        found.untested = untested;
        let root = run(found.command().args(["--getenv", "VERILATOR_ROOT"]))?;
        let root = PathBuf::from(String::from_utf8_lossy(&root.stdout).trim());
        found.version = version;
        found.version_line = version_line;
        found.include_dir = root.join("include");
        Ok(found)
    }

    /// A command running this Verilator in its environment ([`Self::child_env`]).
    pub(crate) fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.envs(self.child_env());
        cmd
    }

    /// The variables every Verilator run gets on top of the inherited environment.
    pub(crate) fn child_env(&self) -> Vec<(OsString, OsString)> {
        self.verilator_root
            .iter()
            .map(|r| (OsString::from("VERILATOR_ROOT"), r.clone().into_os_string()))
            .collect()
    }
}

/// Shell-quotes a command line for error messages, so it can be pasted into a terminal.
pub(crate) fn display_command(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| quote(&a.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=+:,@%".contains(c));
    if plain {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Runs a command to completion; a non-zero exit is an [`Error::Command`].
pub(crate) fn run(cmd: &mut Command) -> Result<Output, Error> {
    let line = display_command(cmd);
    let out = cmd.output().map_err(|source| Error::Spawn {
        command: line.clone(),
        source,
    })?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(Error::command(line, &out))
    }
}

/// The path of a file inside a Verilator output directory.
pub(crate) fn output_file(obj_dir: &Path, prefix: &str, suffix: &str) -> PathBuf {
    obj_dir.join(format!("{prefix}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_are_pasteable() {
        let mut c = Command::new("/bin/verilator");
        c.args(["--cc", "-DX=\"a b\"", "it's"]);
        assert_eq!(
            display_command(&c),
            r#"/bin/verilator --cc '-DX="a b"' 'it'\''s'"#
        );
    }
}
