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
    /// An external tool ran but failed. The message leads with the [`hints`](Self::Command::hints),
    /// if any, then a headline naming what failed (``elaborating DUT `x` ``,
    /// ``verilating DUT `x` ``) and the first error's `file:line:col` and message, then
    /// the rest of the stderr excerpt, a hint read from the stderr when one applies, and
    /// last the command line.
    #[error("{}", display_command_failure(what, command, *status, stderr, path.as_deref(), hints))]
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
        /// What the configuration suggests about the failure, shown first, one `hint:`
        /// line each: an interface parameter default that differs from the module's
        /// when the run stopped on a width diagnostic. Boxed so that `Error` stays under
        /// clippy's `result_large_err` threshold (128 bytes), which a user's function
        /// returning it would otherwise trip.
        hints: Box<Vec<String>>,
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
    ///
    /// Warnings located in `generated` (the generated tops the pass read) are dropped: the
    /// elaboration top leaves every data port open, which `-Wall` reports as one
    /// `PINMISSING` per port, and they are simjig's, not the user's. A pass's warnings
    /// never fail it (`-Wno-fatal`), and the model build lints the wrapper itself. They
    /// are not waived in the generated file instead: `iface_diag` reads the declaration
    /// of an unbound interface port from that `PINMISSING`.
    pub(crate) fn json_pass(dut: &str, e: svtypes::error::Error, generated: &[PathBuf]) -> Self {
        match e {
            svtypes::error::Error::Tool {
                command,
                status,
                stderr,
            } => {
                let stderr = without_warnings_in(&stderr, generated);
                Self::Command {
                    // The JSON pass is named by the command line (`--json-only`); to the
                    // user it is the design's elaboration.
                    what: format!("elaborating DUT `{dut}`"),
                    status,
                    path: offending_path(&stderr),
                    stderr: excerpt(&stderr),
                    command,
                    hints: Box::default(),
                }
            }
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
            hints: Box::default(),
        }
    }

    /// The lines that say why the build failed, short enough to print as warnings above
    /// Cargo's dump of a failed script's stdout: the hints and the headline (the message
    /// up to its first line that is not a `hint:`), then, for a tool run whose headline
    /// could not carry the first diagnostic (a fatal `%Warning-`, which Verilator's
    /// default lint stops on), that diagnostic's line.
    pub(crate) fn summary(&self) -> Vec<String> {
        let text = self.to_string();
        let mut out = Vec::new();
        for line in text.lines() {
            out.push(line.to_owned());
            if !line.starts_with("hint: ") {
                break;
            }
        }
        if let Self::Command { stderr, .. } = self
            && first_error(stderr).and_then(parse_error).is_none()
            && let Some(d) = stderr.lines().find(|l| {
                (l.starts_with("%Error") || l.starts_with("%Warning-"))
                    && !l.starts_with("%Error: Exiting due to")
            })
        {
            out.push(d.to_owned());
        }
        out
    }
}

/// The first `%Error` line of a Verilator `stderr`, which the headline carries.
fn first_error(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .find(|l| is_error_line(l) && !l.starts_with("%Error: Exiting due to"))
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

/// At most this many stderr lines are kept as context (and as many `%Error` lines).
const EXCERPT_LINES: usize = 40;

/// Whether a Verilator stderr line reports an error: a `%Error` line other than the
/// closing summary of a run that stopped on warnings alone (`V3Error.cpp` prints
/// `%Error: Exiting due to N warning(s)` when the error count is 0, and
/// `.. N error(s), M warning(s)` otherwise).
fn is_error_line(line: &str) -> bool {
    line.starts_with("%Error")
        && (!line.starts_with("%Error: Exiting due to") || line.contains("error(s)"))
}

/// The stderr kept in an [`Error::Command`]: all of it when it is short, else every
/// error line (up to [`EXCERPT_LINES`]), then the first [`EXCERPT_LINES`] lines as
/// context. Verilator prints diagnostics in the order it finds them, so warnings (a
/// `-Wall` design's, each several lines long) can fill the first lines before an error.
fn excerpt(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().collect();
    let mut out = String::new();
    if lines.len() > EXCERPT_LINES {
        let errors: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| is_error_line(l))
            .take(EXCERPT_LINES)
            .collect();
        if !errors.is_empty() {
            for line in errors {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("--- the first ");
            out.push_str(&EXCERPT_LINES.to_string());
            out.push_str(" of ");
            out.push_str(&lines.len().to_string());
            out.push_str(" lines:\n");
        }
    }
    for line in lines.iter().take(EXCERPT_LINES) {
        out.push_str(line);
        out.push('\n');
    }
    if lines.len() > EXCERPT_LINES {
        out = format!("{out}... ({} more lines)\n", lines.len() - EXCERPT_LINES);
    }
    out
}

/// `stderr` without the warnings located in `files`. A Verilator diagnostic is a line
/// starting with `%` (`%Warning-CODE: <file>:<line>:<col>: ...`) and the indented lines
/// after it, up to the next `%` line.
fn without_warnings_in(stderr: &str, files: &[PathBuf]) -> String {
    let mut out = String::new();
    let mut dropping = false;
    for line in stderr.lines() {
        if line.starts_with('%') {
            dropping = line.starts_with("%Warning-")
                && line.split_once(": ").is_some_and(|(_, rest)| {
                    files.iter().any(|f| {
                        rest.strip_prefix(f.to_string_lossy().as_ref())
                            .is_some_and(|r| r.starts_with(':'))
                    })
                });
        }
        if !dropping {
            out.push_str(line);
            out.push('\n');
        }
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
fn hint(stderr: &str) -> Option<String> {
    let timing = ["%Warning-STMTDLY", "%Error-STMTDLY", "%Error-NOTIMING"];
    if stderr
        .lines()
        .any(|line| timing.iter().any(|tag| line.starts_with(tag)))
    {
        return Some(
            "the design has `#` delays or event controls that need `--timing`; add \
             `.timing(true)` to its `simjig_build::Dut`"
                .to_owned(),
        );
    }
    if stderr
        .lines()
        .any(|l| l.starts_with("%Error") && PACKAGE_NAME_ERRORS.iter().any(|m| l.contains(m)))
    {
        return Some(
            "if the name comes from a package, is the package imported (`import pkg::*;`, \
             or `pkg::name`), and is its source listed before its users? Verilator reads \
             the file lists in order, then `.sources` in order"
                .to_owned(),
        );
    }
    if let Some(units) = untimed_units(stderr) {
        return Some(format!(
            "some units declare a `timeunit` and these do not: {units}; add a `timeunit` \
             to each, or give every unit a default with \
             `.verilator_args([\"--timescale\", \"1ns/1ps\"])` on the `Dut`"
        ));
    }
    only_lint_warnings(stderr).then(|| {
        "Verilator's lint warnings are fatal by default; fix them, or waive one code with \
         `.verilator_args([\"-Wno-<CODE>\"])` on the `Dut` or a \
         `/* verilator lint_off <CODE> */` comment in the source (the code follows \
         `%Warning-`). `.verilator_args([\"-Wno-fatal\"])` makes every warning non-fatal, \
         but it also hides width mismatches between an interface and the module bound to \
         it, which then build with the port extended or truncated"
            .to_owned()
    })
}

/// Verilator 5.052's errors for a name a package should have declared: a type not found,
/// a `pkg::` scope not found, and a name used before it is declared, which is how an
/// `import pkg::*` from a package read later (or not at all) ends (checked for each).
const PACKAGE_NAME_ERRORS: [&str; 3] = [
    ": Can't find typedef",
    ": Package/class for ",
    " before declaration (IEEE 1800-2023 6.18)",
];

/// The units Verilator's `TIMESCALEMOD` names as missing a `timeunit` (IEEE 1800-2023
/// §3.14.2.3), as `` `file:line` ``, in order and once each: Verilator 5.052 prints
/// `%Warning-TIMESCALEMOD: <file>:<line>:<col>: Timescale missing on this module ...`.
fn untimed_units(stderr: &str) -> Option<String> {
    let mut units: Vec<String> = Vec::new();
    for line in stderr.lines() {
        let Some(rest) = line
            .strip_prefix("%Warning-TIMESCALEMOD: ")
            .or_else(|| line.strip_prefix("%Error-TIMESCALEMOD: "))
        else {
            continue;
        };
        let Some((location, _)) = rest.split_once(": ") else {
            continue;
        };
        // Drop the column: `file:line:col`.
        let unit = location.rsplit_once(':').map_or(location, |(l, _)| l);
        let unit = format!("`{unit}`");
        if !units.contains(&unit) {
            units.push(unit);
        }
    }
    (!units.is_empty()).then(|| units.join(", "))
}

/// Whether Verilator stopped only because of warnings: it printed `%Warning-` lines and no
/// error but its closing `Exiting due to N warning(s)` (`V3Error.cpp`, which prints that
/// line only when the error count is 0).
fn only_lint_warnings(stderr: &str) -> bool {
    let warned = stderr.lines().any(|l| l.starts_with("%Warning-"));
    warned && !stderr.lines().any(is_error_line)
}

/// What [`Error::Command`] displays (see there). `path` is shown when the first error has
/// no location of its own.
fn display_command_failure(
    what: &str,
    command: &str,
    status: Option<i32>,
    stderr: &str,
    path: Option<&Path>,
    hints: &[String],
) -> String {
    let status = status.map_or_else(
        || "killed by a signal".to_owned(),
        |s| format!("exit status: {s}"),
    );
    // The headline (the first line after any hints) is what a build script reporting a
    // handled failure prints, so it carries the first error; the excerpt then shows it
    // once, in the headline.
    let first = first_error(stderr);
    let headline = match first.and_then(parse_error) {
        Some((code, Some(loc), msg)) => {
            let code = code.map(|c| format!("{c}: ")).unwrap_or_default();
            format!("{what} failed ({status}) at {loc}: {code}{msg}")
        }
        Some((code, None, msg)) => {
            let code = code.map(|c| format!("{c}: ")).unwrap_or_default();
            format!("{what} failed ({status}){}: {code}{msg}", on(path))
        }
        None => format!("{what} failed ({status}){}", on(path)),
    };
    let mut out = String::new();
    for h in hints {
        out.push_str("hint: ");
        out.push_str(h);
        out.push('\n');
    }
    out.push_str(&headline);
    // The excerpt without the headline's line (once: the same text may recur).
    let mut rest: Vec<&str> = stderr.lines().collect();
    if let Some(i) = first.and_then(|f| rest.iter().position(|l| *l == f)) {
        rest.remove(i);
    }
    let rest = rest.join("\n");
    if !rest.trim().is_empty() {
        out.push('\n');
        out.push_str(rest.trim_end());
    }
    if let Some(hint) = hint(stderr) {
        out.push_str("\nhint: ");
        out.push_str(&hint);
    }
    out.push_str("\ncommand: ");
    out.push_str(command);
    if command.contains(" -Mdir ") {
        // The JSON pass and the cache verilate into a work directory they remove when
        // the run fails, so the path above no longer exists.
        out.push_str("\n(its `-Mdir` was a temporary directory, removed after the run)");
    }
    out
}

/// The suffix naming `path`, for a headline without a location.
fn on(path: Option<&Path>) -> String {
    path.map(|p| format!(" on `{}`", p.display()))
        .unwrap_or_default()
}

/// The code (`PINNOTFOUND`), location (`file:line:col`) and message of a Verilator error
/// line, `%Error[-CODE]: [<file>:<line>[:<col>]: ]<message>`.
fn parse_error(line: &str) -> Option<(Option<&str>, Option<&str>, &str)> {
    let (tag, rest) = line.strip_prefix("%Error")?.split_once(": ")?;
    let code = tag.strip_prefix('-');
    // The path ends at the first `:` followed by a digit, the location at the `: ` after
    // its `:<line>[:<col>]`.
    let location = rest
        .char_indices()
        .find(|&(i, c)| c == ':' && rest[i + 1..].starts_with(|d: char| d.is_ascii_digit()))
        .and_then(|(i, _)| {
            let digits = rest[i..].find(|c: char| c != ':' && !c.is_ascii_digit())?;
            let end = i + digits;
            (end > 0 && rest[..end].ends_with(':') && rest[end..].starts_with(' '))
                .then(|| (&rest[..end - 1], &rest[end + 1..]))
        });
    Some(match location {
        Some((loc, msg)) => (code, Some(loc), msg),
        None => (code, None, rest),
    })
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
            &[],
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
            &[],
        );
        assert_eq!(
            err.to_string(),
            "elaborating DUT `shreg` failed (exit status: 1) at /r/shreg.sv:3:5: syntax \
             error\ncommand: verilator --json-only shreg.sv"
        );
    }

    /// The configuration's hints come first, one `hint:` line each, so a build script
    /// that prints only a failure's first line prints the most likely cause; the headline
    /// follows them.
    #[test]
    fn hints_lead_the_headline() {
        let err = Error::Command {
            what: "verilating DUT `d`".into(),
            command: "verilator --cc d.sv".into(),
            status: Some(1),
            stderr: "%Error-WIDTHTRUNC: /r/d.sv:2:3: too wide\n".into(),
            path: None,
            hints: Box::new(vec!["one".into(), "two".into()]),
        };
        assert_eq!(
            err.to_string(),
            "hint: one\nhint: two\nverilating DUT `d` failed (exit status: 1) at \
             /r/d.sv:2:3: WIDTHTRUNC: too wide\ncommand: verilator --cc d.sv"
        );
    }

    /// The summary is the hints and the headline, and the first diagnostic only when the
    /// headline cannot carry it: a fatal `%Warning-` under the default lint.
    #[test]
    fn the_summary_is_the_hints_the_headline_and_an_unparsed_diagnostic() {
        let command = |stderr: &str, hints: Vec<String>| Error::Command {
            what: "verilating DUT `d`".into(),
            command: "verilator --cc d.sv".into(),
            status: Some(1),
            stderr: stderr.into(),
            path: None,
            hints: Box::new(hints),
        };
        let lint = "%Warning-WIDTHTRUNC: /r/d.sv:2:3: too wide\n    2 | x\n\
                    %Error: Exiting due to 1 warning(s)\n";
        assert_eq!(
            command(lint, vec!["one".into()]).summary(),
            [
                "hint: one",
                "verilating DUT `d` failed (exit status: 1)",
                "%Warning-WIDTHTRUNC: /r/d.sv:2:3: too wide",
            ]
        );
        let error = "%Error: /r/d.sv:2:3: syntax error\n    2 | x\n";
        assert_eq!(
            command(error, Vec::new()).summary(),
            ["verilating DUT `d` failed (exit status: 1) at /r/d.sv:2:3: syntax error"]
        );
        let config = Error::Config {
            dut: "d".into(),
            msg: "no sources\nhint: add one".into(),
        };
        assert_eq!(
            config.summary(),
            ["invalid DUT `d` configuration: no sources"]
        );
    }

    /// The headline names the location and the error once; the rest of the excerpt (its
    /// source context, later errors) follows it.
    #[test]
    fn the_first_error_is_the_headline_and_is_not_repeated() {
        let err = Error::Command {
            what: "verilating DUT `d`".into(),
            command: "verilator --cc d.sv".into(),
            status: Some(1),
            stderr: "%Error-PINNOTFOUND: /r/d.sv:5:19: Parameter not found: 'X'\n    5 | x\n\
                     %Error: /r/e.sv:1:1: other\n%Error: Exiting due to 2 error(s)\n"
                .into(),
            path: None,
            hints: Box::default(),
        };
        assert_eq!(
            err.to_string(),
            "verilating DUT `d` failed (exit status: 1) at /r/d.sv:5:19: PINNOTFOUND: \
             Parameter not found: 'X'\n    5 | x\n%Error: /r/e.sv:1:1: other\n\
             %Error: Exiting due to 2 error(s)\ncommand: verilator --cc d.sv"
        );
        assert_eq!(
            parse_error("%Error: Parameters from the command line were not found: P"),
            Some((
                None,
                None,
                "Parameters from the command line were not found: P"
            ))
        );
        assert_eq!(
            parse_error("%Error: /a/b c.sv:3: msg: 1"),
            Some((None, Some("/a/b c.sv:3"), "msg: 1"))
        );
    }

    /// The first error is dropped from the excerpt even as its last line without a
    /// newline; a headline without a location names the offending path; and a temporary
    /// `-Mdir` in the command line says it is gone.
    #[test]
    fn the_headline_layout_holds_at_the_edges() {
        let failure = |stderr: &str, command: &str, path: Option<&str>| {
            Error::Command {
                what: "verilating DUT `d`".into(),
                command: command.into(),
                status: Some(1),
                stderr: stderr.into(),
                path: path.map(PathBuf::from),
                hints: Box::default(),
            }
            .to_string()
        };
        assert_eq!(
            failure("%Error: /r/d.sv:3:5: syntax error", "verilator d.sv", None),
            "verilating DUT `d` failed (exit status: 1) at /r/d.sv:3:5: syntax error\n\
             command: verilator d.sv"
        );
        assert_eq!(
            failure(
                "%Error: Cannot find file containing module: 'x'\n%Error: /r/d.sv:1:1: y\n",
                "verilator d.sv",
                Some("/r/d.sv")
            ),
            "verilating DUT `d` failed (exit status: 1) on `/r/d.sv`: Cannot find file \
             containing module: 'x'\n%Error: /r/d.sv:1:1: y\ncommand: verilator d.sv"
        );
        assert!(
            failure("%Error: x", "verilator -Mdir /tmp/w/obj --cc d.sv", None).ends_with(
                "command: verilator -Mdir /tmp/w/obj --cc d.sv\n(its `-Mdir` was a \
                 temporary directory, removed after the run)"
            )
        );
    }

    /// A type or package scope Verilator cannot find (5.052's messages) asks whether the
    /// package is imported and read before its users.
    #[test]
    fn missing_package_names_hint_at_imports_and_order() {
        for stderr in [
            "%Error: /r/u.sv:1:17: Can't find typedef/interface: 'pkt_t'\n",
            "%Error: /r/u.sv:1:17: Package/class for ':: reference' not found: 'p'\n",
            "%Error: /r/u.sv:1:31: Reference to 'pkt_t' before declaration (IEEE 1800-2023 \
             6.18)\n",
        ] {
            let text = Error::Command {
                what: "elaborating DUT `d`".into(),
                command: "verilator d.sv".into(),
                status: Some(1),
                stderr: stderr.into(),
                path: None,
                hints: Box::default(),
            }
            .to_string();
            assert!(
                text.contains("hint: if the name comes from a package, is the package imported"),
                "{text}"
            );
        }
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
                hints: Box::default(),
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
                hints: Box::default(),
            }
            .to_string()
        };
        let warned = "%Warning-WIDTHTRUNC: /r/d.sv:2:12: Operator ASSIGNW expects 4 bits on \
                      the Assign RHS, but Assign RHS's VARREF 'a' generates 8 bits.\n    \
                      2 |   assign y = a;\n                     ... Use \"/* verilator \
                      lint_off WIDTHTRUNC */\" and lint_on around source to disable this \
                      message.\n%Error: Exiting due to 1 warning(s)\n";
        let text = failure(warned);
        // The per-code waivers first; `-Wno-fatal` last, with what it hides.
        let at = |waiver: &str| {
            text.find(waiver)
                .unwrap_or_else(|| panic!("{waiver}: {text}"))
        };
        assert!(
            at("`.verilator_args([\"-Wno-<CODE>\"])`") < at("`.verilator_args([\"-Wno-fatal\"])`")
        );
        assert!(at("lint_off") < at("`.verilator_args([\"-Wno-fatal\"])`"));
        assert!(
            text.contains("also hides width mismatches between an interface and the module"),
            "{text}"
        );
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

    /// A design mixing units with and without a `timeunit` (Verilator 5.052's output,
    /// paths shortened) gets the units to fix and the `--timescale` default, not the
    /// generic lint waiver.
    #[test]
    fn timescalemod_names_the_units_without_a_timeunit() {
        let stderr = "%Warning-TIMESCALEMOD: /r/pkg.sv:1:9: Timescale missing on this module \
                      as other modules have it (IEEE 1800-2023 3.14.2.3)\n    1 | package p;\n\
                      \x20                      /r/top.sv:1:8: ... Location of module with \
                      timescale\n%Warning-TIMESCALEMOD: /r/leaf.sv:1:8: Timescale missing \
                      on this module as other modules have it (IEEE 1800-2023 3.14.2.3)\n\
                      %Error: Exiting due to 2 warning(s)\n";
        let text = Error::Command {
            what: "verilating DUT `d`".into(),
            command: "verilator --cc d.sv".into(),
            status: Some(1),
            stderr: stderr.into(),
            path: None,
            hints: Box::default(),
        }
        .to_string();
        assert!(
            text.contains(
                "hint: some units declare a `timeunit` and these do not: `/r/pkg.sv:1`, \
                 `/r/leaf.sv:1`; add a `timeunit` to each, or give every unit a default \
                 with `.verilator_args([\"--timescale\", \"1ns/1ps\"])` on the `Dut`"
            ),
            "{text}"
        );
        assert!(!text.contains("-Wno-fatal"), "{text}");
    }

    /// Under `-Wall` a page of warnings can come before the error: the excerpt leads with
    /// every error, so the error is shown and the lint hint (for a run that stopped on
    /// warnings alone) is not given.
    #[test]
    fn errors_after_many_warnings_lead_the_excerpt() {
        let mut stderr = String::new();
        for i in 0..30 {
            stderr.push_str("%Warning-PINMISSING: /r/e.sv:5:29: missing pin 'p");
            stderr.push_str(&i.to_string());
            stderr.push_str("'\n    5 | x\n");
        }
        stderr += "%Error: /r/d.sv:39:30: Can't find definition of variable: 'nosuch'\n\
                   %Error: Exiting due to 1 error(s), 30 warning(s)\n";
        let text = Error::Command {
            what: "verilating DUT `d`".into(),
            command: "verilator --cc d.sv".into(),
            status: Some(1),
            stderr: excerpt(&stderr),
            path: offending_path(&stderr),
            hints: Box::default(),
        }
        .to_string();
        assert!(
            text.starts_with(
                "verilating DUT `d` failed (exit status: 1) at /r/d.sv:39:30: Can't find \
                 definition of variable: 'nosuch'\n\
                 %Error: Exiting due to 1 error(s), 30 warning(s)\n\
                 --- the first 40 of 62 lines:\n%Warning-PINMISSING: "
            ),
            "{text}"
        );
        assert!(text.contains("... (22 more lines)"), "{text}");
        assert!(!text.contains("-Wno-fatal"), "{text}");
        // The summary alone says there was an error, though the error line was lost.
        assert!(!only_lint_warnings(
            "%Warning-WIDTH: w\n%Error: Exiting due to 1 error(s), 1 warning(s)\n"
        ));
    }

    /// The warnings located in a generated top go, with their continuation lines; the
    /// user's warnings and every error stay.
    #[test]
    fn warnings_in_generated_tops_are_dropped() {
        let stderr = "%Warning-PINMISSING: /c/simjig_elab_t.sv:5:29: Instance has missing pin: 'clk'\n\
                      \x20   5 |   t dut (.in(in));\n\
                      \x20                  /r/t.sv:3:23: ... Location of port declaration\n\
                      %Warning-UNDRIVEN: /r/if.sv:3:15: Signal is not driven: 'valid'\n\
                      \x20                  : ... note: In instance 'simjig_elab_t.dut'\n\
                      %Error: /c/simjig_elab_t.sv:9:1: syntax error\n\
                      %Error: Exiting due to 1 error(s), 2 warning(s)\n";
        assert_eq!(
            without_warnings_in(stderr, &[PathBuf::from("/c/simjig_elab_t.sv")]),
            "%Warning-UNDRIVEN: /r/if.sv:3:15: Signal is not driven: 'valid'\n\
             \x20                  : ... note: In instance 'simjig_elab_t.dut'\n\
             %Error: /c/simjig_elab_t.sv:9:1: syntax error\n\
             %Error: Exiting due to 1 error(s), 2 warning(s)\n"
        );
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
