//! The single error type of `simjig-build`.
//!
//! Every variant names what failed and the input that made it fail: what a failed tool ran
//! for, the file path it complained about, an excerpt of its stderr and, last, its full
//! command line, so a `build.rs` failure is actionable without rerunning anything by hand.
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
        "`verilator` not found (looked in: {}); install Verilator {} (in the simjig repo: \
         `pixi install --locked`) and put it on PATH, set VERILATOR_ROOT, or set \
         SIMJIG_VERILATOR to the executable",
        display_paths(searched),
        svtypes::version::supported_list()
    )]
    VerilatorNotFound {
        /// Every location that was checked, in order.
        searched: Vec<PathBuf>,
    },
    /// The Verilator found is not in the supported-version table
    /// ([`svtypes::version::SUPPORTED`]).
    #[error(
        "unsupported Verilator at {}: `{found}`; supported versions: {}; install Verilator \
         {} (in the simjig repo: `pixi install --locked`)",
        path.display(),
        svtypes::version::supported_list(),
        svtypes::version::supported_list()
    )]
    UnsupportedVerilator {
        /// The executable that was queried.
        path: PathBuf,
        /// Its `--version` output (first line).
        found: String,
    },
    /// An external tool ran but failed. The message leads with what failed (`JSON pass
    /// for DUT `x``, `verilating DUT `x``) and ends with the command line.
    #[error("{}", display_command_failure(what, command, *status, stderr, path.as_deref()))]
    Command {
        /// What the run was for, e.g. ``verilating DUT `counter` ``.
        what: String,
        /// The full command line, shell-quoted.
        command: String,
        /// The exit code; `None` when a signal ended the process.
        status: Option<i32>,
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
                what: format!("JSON pass for DUT `{dut}`"),
                status,
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

    /// Converts a code generator failure. A probe that does not match the design becomes
    /// [`Error::Config`], worded like the probe checks against Verilator's scope table
    /// (`syms`), so every probe mistake reads alike whichever check found it.
    pub(crate) fn codegen(dut: &str, source: crate::codegen::Error) -> Self {
        match source {
            crate::codegen::Error::Probe { path, reason } => Self::Config {
                dut: dut.to_owned(),
                msg: format!("probe `{path}` (declared in build.rs): {reason}"),
            },
            source => Self::Codegen {
                dut: dut.to_owned(),
                source,
            },
        }
    }

    pub(crate) fn io(op: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io { op, path, source }
    }

    /// Builds a [`Error::Command`] from a finished process run for `what`.
    pub(crate) fn command(what: &str, command: String, output: &Output) -> Self {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Self::Command {
            what: what.to_owned(),
            command,
            status: output.status.code(),
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

/// What to change for a Verilator diagnostic whose fix is a `Dut` option, if `stderr` has
/// one. A DUT is verilated with `--no-timing` unless it asks for `--timing`, and Verilator
/// then ignores a `#` delay (STMTDLY) or rejects an event control inside a procedure
/// (NOTIMING), naming the flag, not the `simjig_build` option.
fn hint(stderr: &str) -> Option<&'static str> {
    let timing = ["%Warning-STMTDLY", "%Error-STMTDLY", "%Error-NOTIMING"];
    if stderr
        .lines()
        .any(|line| timing.iter().any(|tag| line.starts_with(tag)))
    {
        return Some(
            "the design has `#` delays or event controls that need `--timing`; add \
             `.timing(true)` to its `simjig_build::Dut`",
        );
    }
    only_lint_warnings(stderr).then_some(
        "Verilator's lint warnings are fatal by default; fix them, waive one with a \
         `/* verilator lint_off <CODE> */` comment in the source or \
         `.verilator_args([\"-Wno-<CODE>\"])` (the code follows `%Warning-`), or make all \
         warnings non-fatal with `.verilator_args([\"-Wno-fatal\"])` on the `Dut`",
    )
}

/// Whether Verilator stopped only because of warnings: it printed `%Warning-` lines and no
/// error but its closing `Exiting due to N warning(s)` (`V3Error.cpp`, which prints that
/// line only when the error count is 0).
fn only_lint_warnings(stderr: &str) -> bool {
    let warned = stderr.lines().any(|l| l.starts_with("%Warning-"));
    let errored = stderr.lines().any(|l| {
        l.starts_with("%Error")
            && !(l.starts_with("%Error: Exiting due to") && l.ends_with("warning(s)"))
    });
    warned && !errored
}

fn display_command_failure(
    what: &str,
    command: &str,
    status: Option<i32>,
    stderr: &str,
    path: Option<&Path>,
) -> String {
    let status = status.map_or_else(
        || "killed by a signal".to_owned(),
        |s| format!("exit status: {s}"),
    );
    let mut out = format!("{what} failed ({status})");
    if let Some(path) = path {
        out = format!("{out} on `{}`", path.display());
    }
    if !stderr.trim().is_empty() {
        out.push_str("\nstderr:\n");
        out.push_str(stderr.trim_end());
    }
    if let Some(hint) = hint(stderr) {
        out.push_str("\nhint: ");
        out.push_str(hint);
    }
    out.push_str("\ncommand: ");
    out.push_str(command);
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
    fn command_failures_lead_with_what_failed_and_end_with_the_command() {
        let err = Error::json_pass(
            "shreg",
            svtypes::error::Error::Tool {
                command: "verilator --json-only shreg.sv".into(),
                status: Some(1),
                stderr: "%Error: /r/shreg.sv:3:5: syntax error\n".into(),
            },
        );
        assert_eq!(
            err.to_string(),
            "JSON pass for DUT `shreg` failed (exit status: 1) on `/r/shreg.sv`\nstderr:\n\
             %Error: /r/shreg.sv:3:5: syntax error\ncommand: verilator --json-only shreg.sv"
        );
    }

    /// A delay or event control that needs `--timing` (Verilator's STMTDLY warning, its
    /// NOTIMING error) gets the build option to add; other failures get no such hint.
    #[test]
    fn timing_diagnostics_suggest_timing_true() {
        let failure = |stderr: &str| {
            Error::Command {
                what: "verilating DUT `d`".into(),
                command: "verilator --cc --no-timing d.sv".into(),
                status: Some(1),
                stderr: stderr.into(),
                path: None,
            }
            .to_string()
        };
        let hint = "hint: the design has `#` delays or event controls that need `--timing`; \
                    add `.timing(true)` to its `simjig_build::Dut`";
        for stderr in [
            "%Warning-STMTDLY: /r/d.sv:4:5: Ignoring delay on this statement due to --no-timing\n",
            "%Error-NOTIMING: /r/d.sv:7:5: Event control statement in this location requires \
             --timing\n",
        ] {
            let text = failure(stderr);
            assert!(text.contains(hint), "{text}");
            assert!(
                text.ends_with("command: verilator --cc --no-timing d.sv"),
                "{text}"
            );
        }
        let text = failure("%Error: /r/d.sv:3:5: syntax error\n");
        assert!(!text.contains("hint:"), "{text}");
    }

    /// A model that failed only on lint warnings (Verilator 5.052's output for one
    /// WIDTHTRUNC) gets the ways to waive them; a real error does not.
    #[test]
    fn lint_warnings_suggest_waivers_and_wno_fatal() {
        let failure = |stderr: &str| {
            Error::Command {
                what: "verilating DUT `d`".into(),
                command: "verilator --cc d.sv".into(),
                status: Some(1),
                stderr: stderr.into(),
                path: None,
            }
            .to_string()
        };
        let warned = "%Warning-WIDTHTRUNC: /r/d.sv:2:12: Operator ASSIGNW expects 4 bits on \
                      the Assign RHS, but Assign RHS's VARREF 'a' generates 8 bits.\n    \
                      2 |   assign y = a;\n                     ... Use \"/* verilator \
                      lint_off WIDTHTRUNC */\" and lint_on around source to disable this \
                      message.\n%Error: Exiting due to 1 warning(s)\n";
        let text = failure(warned);
        for waiver in [
            "`.verilator_args([\"-Wno-fatal\"])`",
            "`.verilator_args([\"-Wno-<CODE>\"])`",
            "lint_off",
        ] {
            assert!(text.contains(waiver), "{waiver}: {text}");
        }
        // Truncated: the warnings, without the closing line.
        assert!(failure("%Warning-WIDTH: /r/d.sv:4:13: w\n").contains("-Wno-fatal"));
        for stderr in [
            "%Warning-WIDTH: /r/d.sv:4:13: w\n%Error: /r/d.sv:9:1: syntax error\n\
             %Error: Exiting due to 1 error(s), 1 warning(s)\n",
            "%Error: /r/d.sv:3:5: syntax error\n%Error: Exiting due to 1 error(s)\n",
        ] {
            assert!(!failure(stderr).contains("-Wno-fatal"), "{stderr}");
        }
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
