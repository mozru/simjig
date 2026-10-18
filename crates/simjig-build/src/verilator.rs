//! Finding Verilator, checking its version, and running it.
//!
//! The version gate exists because simjig decodes Verilator's output (the `--json-only`
//! AST, the header port macros, `_classes.mk`) and links against its runtime; formats
//! change between releases, so only versions with fixtures in this repository are
//! accepted.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{BuildEnv, Error};

/// A Verilator release number, as in `Verilator 5.052`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VerilatorVersion {
    /// Major version.
    pub major: u32,
    /// Minor version (`52` for `5.052`).
    pub minor: u32,
}

impl fmt::Display for VerilatorVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03}", self.major, self.minor)
    }
}

/// Every Verilator version this release supports. Adding one requires fixtures for it.
pub const SUPPORTED_VERILATOR: &[VerilatorVersion] = &[VerilatorVersion {
    major: 5,
    minor: 52,
}];

impl VerilatorVersion {
    /// Parses the first line of `verilator --version`, e.g.
    /// `Verilator 5.052 2026-09-05 rev conda-forge build`. Development builds
    /// (`5.053-devel`) and anything else unparsable return `None`.
    pub fn parse(version_line: &str) -> Option<Self> {
        let num = version_line
            .trim()
            .strip_prefix("Verilator ")?
            .split_whitespace()
            .next()?;
        let (major, minor) = num.split_once('.')?;
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }

    /// Whether this version is in [`SUPPORTED_VERILATOR`].
    pub fn is_supported(self) -> bool {
        SUPPORTED_VERILATOR.contains(&self)
    }
}

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
}

impl Verilator {
    /// Finds Verilator (`SIMJIG_VERILATOR`, then `$VERILATOR_ROOT/bin/verilator`, then
    /// `PATH`), checks its version and asks it for its include directory.
    pub fn locate(env: &BuildEnv) -> Result<Self, Error> {
        let exe = find_exe(env)?;
        let out = run(Command::new(&exe).arg("--version"))?;
        let version_line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let version = VerilatorVersion::parse(&version_line)
            .filter(|v| v.is_supported())
            .ok_or_else(|| Error::UnsupportedVerilator {
                path: exe.clone(),
                found: version_line.clone(),
                supported: SUPPORTED_VERILATOR,
            })?;
        let root = run(Command::new(&exe).args(["--getenv", "VERILATOR_ROOT"]))?;
        let root = PathBuf::from(String::from_utf8_lossy(&root.stdout).trim());
        Ok(Self {
            exe,
            version,
            version_line,
            include_dir: root.join("include"),
        })
    }
}

fn find_exe(env: &BuildEnv) -> Result<PathBuf, Error> {
    let mut searched = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(env.simjig_verilator.clone());
    candidates.extend(env.verilator_root.as_ref().map(|r| r.join("bin/verilator")));
    // An explicit choice that is wrong must fail, not silently fall through to PATH.
    if let Some(p) = candidates.first() {
        return if crate::env::is_executable(p) {
            Ok(p.clone())
        } else {
            Err(Error::VerilatorNotFound {
                searched: vec![p.clone()],
            })
        };
    }
    for dir in env.path_dirs() {
        let p = dir.join("verilator");
        if crate::env::is_executable(&p) {
            return Ok(p);
        }
        searched.push(p);
    }
    Err(Error::VerilatorNotFound { searched })
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
    fn version_line_parsing() {
        let v = |s: &str| VerilatorVersion::parse(s);
        assert_eq!(
            v("Verilator 5.052 2026-09-05 rev conda-forge build"),
            Some(VerilatorVersion {
                major: 5,
                minor: 52
            })
        );
        assert_eq!(
            v("Verilator 4.228 2022-10-01 rev v4.228"),
            Some(VerilatorVersion {
                major: 4,
                minor: 228
            })
        );
        assert_eq!(v("Verilator 5.053-devel rev v5.052-12-g1"), None);
        assert_eq!(v("verilator: command not found"), None);
        assert_eq!(v(""), None);
    }

    #[test]
    fn version_table() {
        assert!(
            VerilatorVersion {
                major: 5,
                minor: 52
            }
            .is_supported()
        );
        assert!(
            !VerilatorVersion {
                major: 5,
                minor: 50
            }
            .is_supported()
        );
        assert!(
            !VerilatorVersion {
                major: 4,
                minor: 228
            }
            .is_supported()
        );
        assert_eq!(VerilatorVersion { major: 5, minor: 2 }.to_string(), "5.002");
    }

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
