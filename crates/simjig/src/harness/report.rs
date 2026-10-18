//! [`FailureReport`]: the block a failed test prints, and the signature that buckets it.
//!
//! Every failed `#[simjig::test]` prints exactly one block (proposal C §7):
//! ```text
//! FAIL <test>  seed=<seed>  t=<time>  [<Kind> sig=<signature>]
//!   task:     <hierarchical task name>
//!   at:       <file:line:col>
//!   message:  <one line, continuation lines indented>
//!   <kind-specific detail lines>
//!   help:     <what to try>
//!   repro:    SIMJIG_SEED=<seed> [pixi run ]cargo nextest run -p <package>
//!             -E 'binary(=<binary>) & test(=<test>)'   (on one line)
//!   waves:    <the same command with SIMJIG_WAVES=1>
//!   run dir:  <target>/simjig/runs/<package>/<binary>/<test>/<seed>/
//! ```
//! Lines without data (`task`, `at`, `run dir`) are left out.
//!
//! The **signature** hashes the kind, the location and the message with every number
//! masked, so the same bug found with 400 seeds forms one bucket. It deliberately leaves
//! out the time, the seed and the task path, which vary with the seed.

use std::fmt;
use std::path::PathBuf;

use crate::{BlockedTask, Failure, FailureKind, Precision, SimDuration, SimTime};

use super::seed::{Fnv, Seed};

/// A stable hash of a failure's kind, location and number-masked message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Signature(pub u64);

impl Signature {
    /// The signature of a failure of kind `kind` at `location` with `message`.
    #[must_use]
    pub fn of(kind: &str, location: Option<&str>, message: &str) -> Self {
        let mut h = Fnv::default();
        for part in [kind, location.unwrap_or(""), &mask_numbers(message)] {
            h.write(part.as_bytes());
            h.write(&[0]);
        }
        Self(h.finish())
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Replaces every number in `text` with `#`, so the values a failure prints do not split
/// its triage bucket. A number is:
/// - a word that starts with a digit (`42`, `0x1f`, `10ns`, `1_000`);
/// - a sized SystemVerilog literal, `<digits>'[s]<base><digits>` with base `d b h o` in
///   either case (`8'h08`, `4'sb1x0z`): how `simjig_bits` values print;
/// - a word made only of hex digits that contains a digit (`dead1234`).
///
/// Other words (`u8`, `main.t3`, `beef`) are kept.
#[must_use]
pub(crate) fn mask_numbers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if !is_word(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_word(chars[i]) {
            i += 1;
        }
        if chars[start].is_ascii_digit() {
            i = sv_literal_end(&chars, i).unwrap_or(i);
            out.push('#');
            continue;
        }
        let word = &chars[start..i];
        if word.iter().all(char::is_ascii_hexdigit) && word.iter().any(char::is_ascii_digit) {
            out.push('#');
        } else {
            out.extend(word);
        }
    }
    out
}

/// If `chars[at..]` continues a size (`8`) into a SystemVerilog literal (`'h08`, `'sd5`),
/// the index just past the literal.
fn sv_literal_end(chars: &[char], at: usize) -> Option<usize> {
    let mut i = at;
    if chars.get(i) != Some(&'\'') {
        return None;
    }
    i += 1;
    if matches!(chars.get(i), Some('s' | 'S')) {
        i += 1;
    }
    if !matches!(
        chars.get(i),
        Some('d' | 'b' | 'h' | 'o' | 'D' | 'B' | 'H' | 'O')
    ) {
        return None;
    }
    i += 1;
    let digits = chars[i..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
        .count();
    (digits > 0).then_some(i + digits)
}

/// Which test run a report belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunIdentity {
    /// The test's libtest name (`module::test`), as `nextest -E 'test(=...)'` matches it.
    pub test: String,
    /// The Cargo package that holds the test.
    pub package: String,
    /// The run's root seed.
    pub seed: Seed,
    /// Whether the test is `#[ignore]`d, so the repro must ask nextest to run it.
    pub ignored: bool,
    /// What the repro runs `cargo` through (`pixi run`), when the test ran inside an
    /// environment that a plain shell lacks; see `RunEnv::from_process`.
    pub launcher: Option<String>,
    /// The test binary (`harness_tests`), if known.
    pub binary: Option<String>,
}

impl RunIdentity {
    /// The nextest invocation that runs only this test.
    fn nextest(&self) -> String {
        let ignored = if self.ignored {
            " --run-ignored all"
        } else {
            ""
        };
        let launcher = self
            .launcher
            .as_deref()
            .map_or_else(String::new, |l| format!("{l} "));
        // `-p` limits the build; `binary(=...)` keeps a same-named test in another test
        // file of the package out.
        let binary = self
            .binary
            .as_deref()
            .map_or_else(String::new, |b| format!("binary(={b}) & "));
        format!(
            "{launcher}cargo nextest run -p {}{ignored} -E '{binary}test(={})'",
            self.package, self.test
        )
    }

    /// The one-line command that reproduces this run.
    #[must_use]
    pub fn repro(&self) -> String {
        format!("SIMJIG_SEED={} {}", self.seed, self.nextest())
    }

    /// The command that re-runs it with waves (recording is not implemented yet).
    #[must_use]
    pub fn waves(&self) -> String {
        format!(
            "SIMJIG_WAVES=1 SIMJIG_SEED={} {}  (placeholder: wave recording is not \
             implemented yet)",
            self.seed,
            self.nextest()
        )
    }
}

/// The time limit a test ran under, named in its `TimeLimit` report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedTimeout {
    /// The limit.
    pub duration: SimDuration,
    /// Whether it is the default of `#[simjig::test]` (no `timeout` argument), so the
    /// report says how to change it.
    pub is_default: bool,
}

/// Everything a failed test prints, as data; its `Display` is the report block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureReport {
    /// The run that failed.
    pub run: RunIdentity,
    /// The failure kind's name (`TaskPanic`).
    pub kind: &'static str,
    /// When the failure was detected.
    pub time: SimTime,
    /// The design's precision, to print `time`.
    pub precision: Precision,
    /// The hierarchical name of the task that caused it, if one did.
    pub task: Option<String>,
    /// The source location, if known.
    pub location: Option<String>,
    /// What went wrong.
    pub message: String,
    /// Kind-specific lines (blocked tasks, conflicting writers, the time limit, ...).
    pub details: Vec<String>,
    /// What to try next.
    pub help: Option<String>,
    /// The triage bucket.
    pub signature: Signature,
    /// Where the run's `summary.json` and `events.jsonl` were written.
    pub run_dir: Option<PathBuf>,
}

impl FailureReport {
    /// The report of `failure` in `run`.
    ///
    /// `location` overrides the failure's own location (`None` keeps the kernel's).
    /// `timeout` is the test's `timeout`, named in a `TimeLimit` report.
    #[must_use]
    pub fn new(
        failure: &Failure,
        run: RunIdentity,
        location: Option<String>,
        timeout: Option<AppliedTimeout>,
    ) -> Self {
        let location = location.or_else(|| failure.location.map(ToString::to_string));
        let kind = failure.kind.name();
        Self {
            signature: Signature::of(kind, location.as_deref(), &failure.message),
            run,
            kind,
            time: failure.time,
            precision: failure.precision,
            task: failure.task.clone(),
            location,
            message: failure.message.clone(),
            details: details(&failure.kind, failure.precision, timeout),
            help: help(failure),
            run_dir: None,
        }
    }
}

/// The kernel's advice for the failure: the `help:` line its `Display` ends with.
fn help(failure: &Failure) -> Option<String> {
    let text = failure.to_string();
    let (_, help) = text.rsplit_once("\n  help: ")?;
    Some(help.to_owned())
}

/// Kind-specific detail lines, each `label: value`.
fn details(
    kind: &FailureKind,
    precision: Precision,
    timeout: Option<AppliedTimeout>,
) -> Vec<String> {
    match kind {
        FailureKind::TaskPanic { .. }
        | FailureKind::ModelFatal { .. }
        | FailureKind::DpiPanic { .. }
        | FailureKind::DesignFinished => Vec::new(),
        FailureKind::DeltaLimit { limit, signals } => vec![
            format!("limit:    {limit} evals in one timestep"),
            format!("signals:  {}", signals.join(", ")),
        ],
        FailureKind::ReadWriteLimit { limit, waiters } => {
            let mut lines = vec![format!(
                "limit:    {limit} ReadWrite rounds in one timestep"
            )];
            lines.extend(blocked_lines(waiters));
            lines
        }
        FailureKind::WriteConflict {
            signal,
            first,
            second,
        } => vec![
            format!("signal:   {signal}"),
            // The kernel's labels are already quoted (`task `main.a``, `the kernel`).
            format!("writers:  {first}, then {second}"),
        ],
        FailureKind::WriteInReadOnly { signal } => vec![format!("signal:   {signal}")],
        FailureKind::Starved { blocked } | FailureKind::Deadlock { blocked } => {
            blocked_lines(blocked)
        }
        FailureKind::TimeLimit { limit, blocked } => {
            let mut lines = vec![format!("limit:    t={}", limit.display(precision))];
            if let Some(timeout) = timeout {
                let from = if timeout.is_default {
                    "(the default for #[simjig::test]; change it with \
                     `#[simjig::test(timeout = \"50ms\")]`, or `timeout = \"none\"` for no limit)"
                } else {
                    "(from `#[simjig::test(timeout = ...)]`)"
                };
                lines.push(format!(
                    "timeout:  {} {from}",
                    SimTime(timeout.duration.ticks()).display(precision)
                ));
            }
            lines.extend(blocked_lines(blocked));
            lines
        }
    }
}

/// One `blocked:` line per task: what it awaits.
fn blocked_lines(blocked: &[BlockedTask]) -> Vec<String> {
    blocked
        .iter()
        .map(|b| {
            let awaiting = if b.awaiting.is_empty() {
                "a future the kernel cannot see".to_owned()
            } else {
                b.awaiting.join(", ")
            };
            format!("blocked:  `{}` awaits {awaiting}", b.task)
        })
        .collect()
}

impl fmt::Display for FailureReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "FAIL {}  seed={}  t={}  [{} sig={}]",
            self.run.test,
            self.run.seed,
            self.time.display(self.precision),
            self.kind,
            self.signature
        )?;
        if let Some(task) = &self.task {
            writeln!(f, "  task:     {task}")?;
        }
        if let Some(location) = &self.location {
            writeln!(f, "  at:       {location}")?;
        }
        writeln!(
            f,
            "  message:  {}",
            self.message.replace('\n', "\n            ")
        )?;
        for line in &self.details {
            writeln!(f, "  {line}")?;
        }
        if let Some(help) = &self.help {
            writeln!(f, "  help:     {help}")?;
        }
        writeln!(f, "  repro:    {}", self.run.repro())?;
        write!(f, "  waves:    {}", self.run.waves())?;
        if let Some(dir) = &self.run_dir {
            write!(f, "\n  run dir:  {}", dir.display())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking_hides_numbers_but_keeps_names() {
        assert_eq!(
            mask_numbers("got 0x1f at 10ns (1_000 cycles), u8 main.t3 dead1234 beef"),
            "got # at # (# cycles), u8 main.t3 # beef"
        );
        assert_eq!(
            mask_numbers("8'h08 != 8'hff, 4'sb1x0z 32'D7 9'h0_05; 3' 2'q it's"),
            "# != #, # # #; #' #'q it's"
        );
    }

    #[test]
    fn signature_ignores_numbers_but_not_kind_or_location() {
        let a = Signature::of("TaskPanic", Some("a.rs:1:1"), "value 17 != 18");
        assert_eq!(
            a,
            Signature::of("TaskPanic", Some("a.rs:1:1"), "value 3 != 4")
        );
        assert_ne!(
            a,
            Signature::of("ModelFatal", Some("a.rs:1:1"), "value 3 != 4")
        );
        assert_ne!(
            a,
            Signature::of("TaskPanic", Some("b.rs:1:1"), "value 3 != 4")
        );
        assert_ne!(
            a,
            Signature::of("TaskPanic", Some("a.rs:1:1"), "other 3 != 4")
        );
    }
}
