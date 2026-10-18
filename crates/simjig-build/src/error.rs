//! The single error type of `simjig-build`.
//!
//! Every variant names what failed and the input that made it fail: the full command line
//! of a failed tool, an excerpt of its stderr, and the file path it complained about, so a
//! `build.rs` failure is actionable without rerunning anything by hand.
//!
//! A wrapped cause is the [`source`](std::error::Error::source), never part of the
//! message, so a reporter that walks the chain prints it once. `Debug` is the message
//! followed by that chain, because `fn main() -> Result<(), Error>` prints `Debug`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::header::PortMismatch;

/// Errors from configuring, verilating or compiling a DUT.
#[derive(thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The `Dut` configuration is invalid (for example an empty source list).
    #[error("invalid DUT `{dut}` configuration: {msg}")]
    Config {
        /// The DUT name given to `Dut::new`.
        dut: String,
        /// What is wrong and how to fix it.
        msg: String,
    },
    /// A variable that Cargo sets for build scripts is missing.
    #[error(
        "environment variable `{var}` is not set; `simjig_build::Dut::build` must run inside a \
         Cargo build script (use `Dut::build_in` with an explicit `BuildEnv` elsewhere)"
    )]
    MissingEnv {
        /// The variable name.
        var: &'static str,
    },
    /// An environment variable simjig reads has a value it does not accept.
    #[error("environment variable `{var}` is `{value}`; expected {expected}")]
    BadEnv {
        /// The variable name.
        var: &'static str,
        /// Its value.
        value: String,
        /// What it may be.
        expected: &'static str,
    },
    /// No `verilator` executable was found.
    #[error(
        "`verilator` not found (looked in: {}); install it (e.g. `pixi install`), put it on \
         PATH, set VERILATOR_ROOT, or set SIMJIG_VERILATOR to the executable",
        display_paths(searched)
    )]
    VerilatorNotFound {
        /// Every location that was checked, in order.
        searched: Vec<PathBuf>,
    },
    /// The Verilator found is not in the supported-version table
    /// ([`svtypes::version::SUPPORTED`]).
    #[error(
        "unsupported Verilator at {}: `{found}`; supported versions: {}",
        path.display(),
        svtypes::version::supported_list()
    )]
    UnsupportedVerilator {
        /// The executable that was queried.
        path: PathBuf,
        /// Its `--version` output (first line).
        found: String,
    },
    /// An external tool ran but failed.
    #[error("{}", display_command_failure(command, status, stderr, path.as_deref()))]
    Command {
        /// The full command line, shell-quoted.
        command: String,
        /// How the process ended.
        status: String,
        /// The first lines of its stderr.
        stderr: String,
        /// The source file the tool complained about, when it named one.
        path: Option<PathBuf>,
    },
    /// An external tool could not be started. The OS error is the
    /// [`source`](std::error::Error::source), not part of the message.
    #[error("could not run `{command}`")]
    Spawn {
        /// The full command line, shell-quoted.
        command: String,
        /// The OS error.
        source: std::io::Error,
    },
    /// A filesystem operation failed. The OS error is the
    /// [`source`](std::error::Error::source), not part of the message.
    #[error("{op} `{}`", path.display())]
    Io {
        /// What was being done (`"reading"`, `"renaming into"`, ...).
        op: &'static str,
        /// The path the operation was applied to.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
    /// A file that Verilator generated did not have the expected shape.
    #[error("{}:{line}: {msg}", path.display())]
    Parse {
        /// The generated file (`.d`, `_classes.mk` or `V<prefix>.h`).
        path: PathBuf,
        /// 1-based line number, or 0 when the whole file is at fault.
        line: usize,
        /// What was expected and what was found.
        msg: String,
    },
    /// Compiling the Verilator output or libverilated failed. The `cc` error is the
    /// [`source`](std::error::Error::source), not part of the message.
    #[error("compiling {what} failed")]
    Compile {
        /// Which library was being built.
        what: String,
        /// The `cc` error; its message carries the compiler command line and output.
        source: cc::Error,
    },
    /// The JSON pass (`svtypes`) could not decode the design. The cause is the
    /// [`source`](std::error::Error::source), not part of the message.
    #[error("reading the type IR of DUT `{dut}`")]
    Ir {
        /// The DUT name.
        dut: String,
        /// The `svtypes` error; it names the JSON node and the source location.
        source: svtypes::error::Error,
    },
    /// The code generator cannot handle this design or configuration. The cause is the
    /// [`source`](std::error::Error::source), not part of the message.
    #[error("generating code for DUT `{dut}`")]
    Codegen {
        /// The DUT name.
        dut: String,
        /// What is unsupported or misconfigured.
        source: crate::codegen::Error,
    },
    /// A custom emitter cannot generate code for this design.
    #[error("generating code for DUT `{dut}`: {msg}")]
    Emit {
        /// The DUT name.
        dut: String,
        /// What is unsupported or wrong, and how to fix it.
        msg: String,
    },
    /// The ports in the generated header disagree with the expected ports.
    #[error(transparent)]
    Ports(#[from] PortMismatch),
}

impl Error {
    /// Converts a JSON pass failure. A failed Verilator run becomes [`Error::Command`],
    /// like every other failed tool, so it names the offending source file.
    pub(crate) fn json_pass(dut: &str, e: svtypes::error::Error) -> Self {
        match e {
            svtypes::error::Error::Tool {
                command,
                status,
                stderr,
            } => Self::Command {
                status: status.map_or_else(
                    || "killed by a signal".to_owned(),
                    |s| format!("exit status: {s}"),
                ),
                path: offending_path(&stderr),
                stderr: excerpt(&stderr),
                command,
            },
            source => Self::Ir {
                dut: dut.to_owned(),
                source,
            },
        }
    }

    pub(crate) fn io(op: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io { op, path, source }
    }

    /// Builds a [`Error::Command`] from a finished process.
    pub(crate) fn command(command: String, output: &Output) -> Self {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Self::Command {
            command,
            status: output.status.to_string(),
            path: offending_path(&stderr),
            stderr: excerpt(&stderr),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_chain(self, f)
    }
}

/// The `Debug` of every public error type: the message, then each cause on its own
/// `Caused by:` line, so a `main` returning the error prints it readably.
pub(crate) fn debug_chain(e: &dyn std::error::Error, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{e}")?;
    let mut cause = e.source();
    if cause.is_some() {
        write!(f, "\n\nCaused by:")?;
    }
    while let Some(c) = cause {
        write!(f, "\n    {}", c.to_string().replace('\n', "\n    "))?;
        cause = c.source();
    }
    Ok(())
}

/// At most this many stderr lines are kept; Verilator prints its errors first.
const EXCERPT_LINES: usize = 40;

fn excerpt(stderr: &str) -> String {
    let mut out = String::new();
    let total = stderr.lines().count();
    for line in stderr.lines().take(EXCERPT_LINES) {
        out.push_str(line);
        out.push('\n');
    }
    if total > EXCERPT_LINES {
        out = format!("{out}... ({} more lines)\n", total - EXCERPT_LINES);
    }
    out
}

/// Extracts the file named by the first Verilator diagnostic, e.g. `/a/b.sv` from
/// `%Error: /a/b.sv:3:5: syntax error`.
pub(crate) fn offending_path(stderr: &str) -> Option<PathBuf> {
    stderr.lines().find_map(|line| {
        let rest = line.strip_prefix("%Error")?;
        // Skip the optional `-CODE` tag, then the `: ` separator.
        let (_, rest) = rest.split_once(": ")?;
        let end = rest
            .char_indices()
            .find(|&(i, c)| c == ':' && rest[i + 1..].starts_with(|d: char| d.is_ascii_digit()))?
            .0;
        let path = &rest[..end];
        (!path.is_empty()).then(|| PathBuf::from(path))
    })
}

fn display_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "nowhere: PATH is empty".to_owned();
    }
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn display_command_failure(
    command: &str,
    status: &str,
    stderr: &str,
    path: Option<&Path>,
) -> String {
    let mut out = format!("`{command}` failed ({status})");
    if let Some(path) = path {
        out = format!("{out} on `{}`", path.display());
    }
    if !stderr.trim().is_empty() {
        out.push_str("\nstderr:\n");
        out.push_str(stderr.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offending_path_from_verilator_diagnostics() {
        let check = |stderr: &str, want: Option<&str>| {
            assert_eq!(offending_path(stderr), want.map(PathBuf::from), "{stderr}");
        };
        check("%Error: /a/b.sv:3:5: syntax error", Some("/a/b.sv"));
        check(
            "%Warning-WIDTH: x.sv:1:2: w\n%Error-PINMISSING: /r/top.sv:10:1: missing",
            Some("/r/top.sv"),
        );
        check("%Error: Cannot find file containing module: 'foo'", None);
        check("some other text", None);
    }

    #[test]
    fn ir_errors_keep_their_cause_out_of_the_message() {
        let err = Error::json_pass(
            "d",
            svtypes::error::Error::UnsupportedVersion {
                version: "Verilator 9.000".into(),
            },
        );
        assert_eq!(err.to_string(), "reading the type IR of DUT `d`");
        let source = std::error::Error::source(&err).map(ToString::to_string);
        assert!(source.is_some_and(|s| s.contains("Verilator 9.000")));
    }

    fn os_error() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no such thing")
    }

    #[test]
    fn io_and_spawn_errors_keep_their_cause_out_of_the_message() {
        let io = Error::io("reading", Path::new("/a/b.sv"))(os_error());
        let spawn = Error::Spawn {
            command: "verilator --version".into(),
            source: os_error(),
        };
        for (err, msg) in [
            (io, "reading `/a/b.sv`"),
            (spawn, "could not run `verilator --version`"),
        ] {
            assert_eq!(err.to_string(), msg);
            let source = std::error::Error::source(&err).map(ToString::to_string);
            assert_eq!(source.as_deref(), Some("no such thing"));
        }
    }

    #[test]
    fn debug_is_the_message_and_its_causes_on_separate_lines() {
        let err = Error::io("reading", Path::new("/a/b.sv"))(os_error());
        assert_eq!(
            format!("{err:?}"),
            "reading `/a/b.sv`\n\nCaused by:\n    no such thing"
        );
        let codegen = crate::codegen::Error::Config {
            item: "probe t.x".into(),
            reason: "line one\nline two".into(),
        };
        assert_eq!(format!("{codegen:?}"), codegen.to_string());
        let wrapped = Error::Codegen {
            dut: "d".into(),
            source: codegen,
        };
        assert_eq!(
            format!("{wrapped:?}"),
            "generating code for DUT `d`\n\nCaused by:\n    codegen: probe t.x: line one\n    \
             line two"
        );
        let ports = Error::Ports(PortMismatch {
            problems: Vec::new(),
        });
        let debug = format!("{ports:?}");
        assert_eq!(debug, ports.to_string());
        assert!(debug.contains('\n') && !debug.contains("\\n"), "{debug}");
    }

    #[test]
    fn excerpt_truncates_long_stderr() {
        let long = (0..100).fold(String::new(), |s, i| s + &format!("line {i}\n"));
        let e = excerpt(&long);
        assert!(e.starts_with("line 0\n"));
        assert!(e.contains("line 39\n"));
        assert!(!e.contains("line 40\n"));
        assert!(e.ends_with("(60 more lines)\n"));
    }
}
