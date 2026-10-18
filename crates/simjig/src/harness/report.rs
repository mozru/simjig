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
//!   repro:    SIMJIG_SEED=<seed> cargo nextest run -p <package> -E 'test(=<test>)'
//!   waves:    <the same command with SIMJIG_WAVES=1>
//!   run dir:  <target>/simjig/runs/<test>/<seed>/
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

/// Replaces every number in `text` with `#`: words that start with a digit (`42`,
/// `0x1f`, `10ns`, `1_000`) and words made only of hex digits that contain a digit
/// (`dead1234`). Other words (`u8`, `main.t3`) are kept.
#[must_use]
pub(crate) fn mask_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let starts_with_digit = word.starts_with(|c: char| c.is_ascii_digit());
        let hexlike =
            word.chars().all(|c| c.is_ascii_hexdigit()) && word.chars().any(|c| c.is_ascii_digit());
        if starts_with_digit || hexlike {
            out.push('#');
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
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
}

impl RunIdentity {
    /// The nextest invocation that runs only this test.
    fn nextest(&self) -> String {
        let ignored = if self.ignored {
            " --run-ignored all"
        } else {
            ""
        };
        format!(
            "cargo nextest run -p {}{ignored} -E 'test(={})'",
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
    /// `location` overrides the failure's own location (the harness passes the panic
    /// location of a `TaskPanic`, which the kernel does not capture). `timeout` is the
    /// test's `timeout`, named in a `TimeLimit` report.
    #[must_use]
    pub fn new(
        failure: &Failure,
        run: RunIdentity,
        location: Option<String>,
        timeout: Option<SimDuration>,
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
fn details(kind: &FailureKind, precision: Precision, timeout: Option<SimDuration>) -> Vec<String> {
    match kind {
        FailureKind::TaskPanic { .. }
        | FailureKind::ModelFatal { .. }
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
            format!("writers:  `{first}`, then `{second}`"),
        ],
        FailureKind::WriteInReadOnly { signal } => vec![format!("signal:   {signal}")],
        FailureKind::Starved { blocked } | FailureKind::Deadlock { blocked } => {
            blocked_lines(blocked)
        }
        FailureKind::TimeLimit { limit, blocked } => {
            let mut lines = vec![format!("limit:    t={}", limit.display(precision))];
            if let Some(timeout) = timeout {
                lines.push(format!(
                    "timeout:  {} (from `#[simjig::test(timeout = ...)]`)",
                    SimTime(timeout.ticks()).display(precision)
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
