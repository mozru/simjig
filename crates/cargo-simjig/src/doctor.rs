//! `cargo simjig doctor`: checks that this machine can build and run simjig testbenches.
//!
//! A run has two phases. [`Report::gather`] probes the machine once (runs `verilator
//! --version`, reads `VERILATOR_ROOT`, ...) into plain facts, then pure check functions
//! turn those facts into one [`Finding`] per check. Only the first phase touches the
//! machine, so every decision is unit-tested with fake command outputs.
//!
//! Invariant: every finding that is not [`Status::Ok`] carries a one-line fix command.

mod checks;
mod facts;

use std::fmt;
use std::io::{self, Write as _};
use std::process::ExitCode;

use crate::json;

/// How [`run`] prints the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// [`Report`]'s `Display` form, for people.
    Text,
    /// [`Report::to_json`], for CI and scripts.
    Json,
}

/// Runs every check, prints the report to stdout and returns the process exit code:
/// failure when any check failed or stdout could not be written.
pub fn run(format: Format) -> ExitCode {
    let report = Report::gather();
    let text = match format {
        Format::Text => report.to_string(),
        Format::Json => report.to_json(),
    };
    if let Err(error) = io::stdout().lock().write_all(text.as_bytes()) {
        eprintln!("error: cannot write the doctor report: {error}");
        return ExitCode::FAILURE;
    }
    if report.has_failures() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// The check passed.
    Ok,
    /// Builds still work, but slower or less reproducibly.
    Warn,
    /// simjig will not build or run correctly until this is fixed.
    Fail,
}

impl Status {
    /// The lower-case name used in `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        })
    }
}

/// Which check produced a finding. Variants are listed in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    /// Verilator is on `PATH` and its version is in [`crate::verilator::SUPPORTED`].
    Verilator,
    /// `VERILATOR_ROOT` is unset or names the root the `verilator` binary uses.
    VerilatorRoot,
    /// The Verilator include directory has the FST tracing sources.
    Fst,
    /// The C++ compiler in `CXX` runs, and its version.
    Cxx,
    /// ccache is installed.
    Ccache,
    /// cargo-nextest is installed.
    Nextest,
    /// The current directory and the Verilator install contain no spaces.
    Paths,
    /// The tools come from a pixi environment, or are on `PATH` some other way.
    Environment,
}

impl Check {
    /// The stable identifier used in output, such as `verilator-root`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verilator => "verilator",
            Self::VerilatorRoot => "verilator-root",
            Self::Fst => "fst",
            Self::Cxx => "cxx",
            Self::Ccache => "ccache",
            Self::Nextest => "nextest",
            Self::Paths => "paths",
            Self::Environment => "environment",
        }
    }
}

/// The result of one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    check: Check,
    status: Status,
    summary: String,
    fix: Option<String>,
}

impl Finding {
    fn ok(check: Check, summary: impl Into<String>) -> Self {
        Self {
            check,
            status: Status::Ok,
            summary: summary.into(),
            fix: None,
        }
    }

    fn warn(check: Check, summary: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            check,
            status: Status::Warn,
            summary: summary.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(check: Check, summary: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            check,
            status: Status::Fail,
            summary: summary.into(),
            fix: Some(fix.into()),
        }
    }

    /// Which check this is.
    #[must_use]
    pub fn check(&self) -> Check {
        self.check
    }

    /// Whether the check passed.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status
    }

    /// One line describing what was found.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// The command that fixes the problem; `None` exactly when the status is OK.
    #[must_use]
    pub fn fix(&self) -> Option<&str> {
        self.fix.as_deref()
    }
}

/// All findings of one doctor run, in [`Check`] order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    findings: Vec<Finding>,
}

impl Report {
    /// Probes this machine and runs every check.
    ///
    /// This runs `verilator`, the C++ compiler, `ccache` and `cargo nextest` with
    /// `--version`, and reads the environment and the current directory.
    #[must_use]
    pub fn gather() -> Self {
        let facts = facts::Facts::gather();
        Self {
            findings: checks::run_all(&facts, &|path| path.exists()),
        }
    }

    /// The findings, one per check.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Whether any check failed; the command then exits non-zero.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.findings.iter().any(|f| f.status == Status::Fail)
    }

    /// Machine-readable output, a single JSON object:
    /// `{"ok":bool,"checks":[{"check":str,"status":"ok"|"warn"|"fail","summary":str,"fix":str|null}]}`.
    /// `ok` is false exactly when a check failed.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = format!("{{\"ok\":{},\"checks\":[", !self.has_failures());
        for (i, finding) in self.findings.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"check\":");
            json::push_str(&mut out, finding.check.as_str());
            out.push_str(",\"status\":");
            json::push_str(&mut out, finding.status.as_str());
            out.push_str(",\"summary\":");
            json::push_str(&mut out, &finding.summary);
            out.push_str(",\"fix\":");
            match &finding.fix {
                Some(fix) => json::push_str(&mut out, fix),
                None => out.push_str("null"),
            }
            out.push('}');
        }
        out.push_str("]}\n");
        out
    }
}

/// Human-readable output: one line per check, a `fix:` line under each problem, and a
/// count of each status.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for finding in &self.findings {
            let status = finding.status.to_string();
            let check = finding.check.as_str();
            writeln!(f, "{status:<4} {check:<14} {}", finding.summary)?;
            if let Some(fix) = &finding.fix {
                writeln!(f, "{:19}fix: {fix}", "")?;
            }
        }
        let count = |status| self.findings.iter().filter(|f| f.status == status).count();
        writeln!(
            f,
            "doctor: {} ok, {} warn, {} fail",
            count(Status::Ok),
            count(Status::Warn),
            count(Status::Fail)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report {
            findings: vec![
                Finding::ok(Check::Verilator, "Verilator 5.052 at /x/verilator"),
                Finding::warn(Check::Ccache, "ccache not found", "pixi install --locked"),
            ],
        }
    }

    #[test]
    fn text_shows_status_check_and_fix() {
        assert_eq!(
            sample().to_string(),
            "OK   verilator      Verilator 5.052 at /x/verilator\n\
             WARN ccache         ccache not found\n\
             \x20                  fix: pixi install --locked\n\
             doctor: 1 ok, 1 warn, 0 fail\n"
        );
    }

    #[test]
    fn json_has_fixed_shape() {
        assert_eq!(
            sample().to_json(),
            "{\"ok\":true,\"checks\":[\
             {\"check\":\"verilator\",\"status\":\"ok\",\"summary\":\"Verilator 5.052 at /x/verilator\",\"fix\":null},\
             {\"check\":\"ccache\",\"status\":\"warn\",\"summary\":\"ccache not found\",\"fix\":\"pixi install --locked\"}]}\n"
        );
    }

    #[test]
    fn only_fail_makes_the_run_fail() {
        let mut report = sample();
        assert!(!report.has_failures());
        report
            .findings
            .push(Finding::fail(Check::Paths, "space", "mv a b"));
        assert!(report.has_failures());
        assert!(report.to_json().starts_with("{\"ok\":false,"));
    }
}
