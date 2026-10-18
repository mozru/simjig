//! [`Failure`]: why a simulation run stopped, as data.
//!
//! The kernel stops at the first failure, so the trace ends at the cause. Every failure
//! records when (simulation time), who (the hierarchical task name), where (the source
//! location, when one is known), and a `Display` that says what to do next.

use std::fmt;
use std::panic::Location;
use std::sync::{Mutex, PoisonError};

use super::time::{Precision, SimTime};

/// A source position (`file:line:column`) that a failure points at: the user's call that
/// broke a rule, or the line that panicked.
///
/// Locations are interned for the life of the process, so a [`Failure`] can hold one as a
/// `&'static` reference whether it came from `#[track_caller]` (already `'static`) or from
/// a panic hook (whose location borrows the panic). Interning leaks one small record per
/// distinct position, which is bounded by the program's source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceLocation {
    file: &'static str,
    line: u32,
    column: u32,
}

/// Every location interned so far; searched linearly, since only failures intern.
static INTERNED: Mutex<Vec<&'static SourceLocation>> = Mutex::new(Vec::new());

impl SourceLocation {
    /// The source file, as the compiler names it.
    #[must_use]
    pub const fn file(&self) -> &'static str {
        self.file
    }

    /// The 1-based line.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.line
    }

    /// The 1-based column.
    #[must_use]
    pub const fn column(&self) -> u32 {
        self.column
    }

    /// The interned copy of `location`.
    pub(crate) fn intern(location: &Location<'_>) -> &'static Self {
        let mut interned = INTERNED.lock().unwrap_or_else(PoisonError::into_inner);
        let same = |l: &&&'static Self| {
            l.file == location.file() && l.line == location.line() && l.column == location.column()
        };
        if let Some(found) = interned.iter().find(same) {
            return found;
        }
        let file = match interned.iter().find(|l| l.file == location.file()) {
            Some(l) => l.file,
            None => Box::leak(location.file().into()),
        };
        let new: &'static Self = Box::leak(Box::new(Self {
            file,
            line: location.line(),
            column: location.column(),
        }));
        interned.push(new);
        new
    }
}

impl From<&'static Location<'static>> for &'static SourceLocation {
    fn from(location: &'static Location<'static>) -> Self {
        SourceLocation::intern(location)
    }
}

impl fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// A task that could not make progress, and what it was waiting for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedTask {
    /// Hierarchical task name (`main.driver`).
    pub task: String,
    /// One description per trigger the task is registered on (`rising(clk)`); empty if the
    /// task awaits something the kernel cannot see.
    pub awaiting: Vec<String>,
}

/// The kind of failure, with the data specific to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// A task panicked.
    TaskPanic {
        /// The panic message.
        message: String,
    },
    /// Writes kept arriving: the timestep did not settle within the delta limit.
    DeltaLimit {
        /// The delta limit that was hit.
        limit: u32,
        /// Signals written in the last delta.
        signals: Vec<String>,
    },
    /// Two writers wrote one signal in the same delta.
    WriteConflict {
        /// The signal.
        signal: String,
        /// The writer that wrote first: a task's hierarchical name (`main.a`), or
        /// `clock` for a kernel clock, or `kernel` for writes outside any task.
        first: String,
        /// The writer that wrote second, named like `first`.
        second: String,
    },
    /// A task wrote a signal during the `ReadOnly` phase.
    WriteInReadOnly {
        /// The signal.
        signal: String,
    },
    /// Tasks wait on signals or time, but nothing will ever change: no timer, clock or
    /// model event is scheduled.
    Starved {
        /// Every blocked task.
        blocked: Vec<BlockedTask>,
    },
    /// Tasks wait only on each other (events, locks, queues, task results).
    Deadlock {
        /// Every blocked task.
        blocked: Vec<BlockedTask>,
    },
    /// The model reported a fatal error during eval.
    ModelFatal {
        /// The model's message.
        message: String,
    },
    /// The design executed `$finish` before the main task returned.
    DesignFinished,
    /// The next timestep is past the limit set with
    /// [`Sim::set_time_limit`](crate::Sim::set_time_limit).
    TimeLimit {
        /// The limit.
        limit: SimTime,
    },
}

impl FailureKind {
    /// The variant name, for one-line summaries and triage buckets.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TaskPanic { .. } => "TaskPanic",
            Self::DeltaLimit { .. } => "DeltaLimit",
            Self::WriteConflict { .. } => "WriteConflict",
            Self::WriteInReadOnly { .. } => "WriteInReadOnly",
            Self::Starved { .. } => "Starved",
            Self::Deadlock { .. } => "Deadlock",
            Self::ModelFatal { .. } => "ModelFatal",
            Self::DesignFinished => "DesignFinished",
            Self::TimeLimit { .. } => "TimeLimit",
        }
    }

    fn help(&self) -> &'static str {
        match self {
            Self::TaskPanic { .. } => "the panic message and the stderr backtrace show the cause",
            Self::DeltaLimit { .. } => {
                "a task re-writes a signal every delta (often in reaction to its own write); \
                 await an edge or a Timer between writes"
            }
            Self::WriteConflict { .. } => {
                "only one task may drive a signal per delta; give the signal a single owner \
                 task or serialize the writers with a Lock"
            }
            Self::WriteInReadOnly { .. } => {
                "ReadOnly is for sampling settled values; await a Timer, an edge or \
                 NextTimeStep before writing"
            }
            Self::Starved { .. } => {
                "start a clock, await a Timer, or check that the awaited signal is driven"
            }
            Self::Deadlock { .. } => {
                "each listed task waits on another; check lock order and that events are set"
            }
            Self::ModelFatal { .. } => "the design reported the error; see its message",
            Self::DesignFinished => {
                "the design called $finish; end the test before it, or remove the $finish"
            }
            Self::TimeLimit { .. } => "the test ran too long; a task is probably waiting forever",
        }
    }
}

/// Why a simulation run stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What went wrong.
    pub kind: FailureKind,
    /// Simulation time when it was detected.
    pub time: SimTime,
    /// The design's precision, to print `time` in physical units.
    pub precision: Precision,
    /// Hierarchical name of the task that caused it, if one did.
    pub task: Option<String>,
    /// Source location of the offending call or of the panic, if known.
    pub location: Option<&'static SourceLocation>,
    /// One-line description.
    pub message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "simjig {} at t={}",
            self.kind.name(),
            self.time.display(self.precision)
        )?;
        if let Some(task) = &self.task {
            write!(f, " in task `{task}`")?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(location) = self.location {
            write!(f, "\n  at {location}")?;
        }
        if let FailureKind::Starved { blocked } | FailureKind::Deadlock { blocked } = &self.kind {
            for b in blocked {
                if b.awaiting.is_empty() {
                    write!(
                        f,
                        "\n  task `{}` awaits a future the kernel cannot see",
                        b.task
                    )?;
                } else {
                    write!(f, "\n  task `{}` awaits {}", b.task, b.awaiting.join(", "))?;
                }
            }
        }
        if let Some(help) = self.help() {
            write!(f, "\n  help: {help}")?;
        }
        Ok(())
    }
}

impl Failure {
    /// What to do about the failure: one line of advice, the `help:` line of the
    /// `Display`. Every kind has one today; `None` leaves room for kinds without advice.
    #[must_use]
    pub fn help(&self) -> Option<&str> {
        Some(self.kind.help())
    }
}

impl std::error::Error for Failure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interned_locations_print_like_std_and_are_shared() {
        let here = Location::caller();
        let a: &'static SourceLocation = here.into();
        let b = SourceLocation::intern(here);
        assert!(std::ptr::eq(a, b));
        assert_eq!(a.to_string(), here.to_string());
        assert_eq!(
            (a.file(), a.line(), a.column()),
            (here.file(), here.line(), here.column())
        );
    }

    #[test]
    fn display_names_kind_time_task_and_waits() {
        let failure = Failure {
            kind: FailureKind::Deadlock {
                blocked: vec![BlockedTask {
                    task: "main.a".into(),
                    awaiting: vec!["Lock `bus`".into()],
                }],
            },
            time: SimTime(5),
            precision: Precision::NS,
            task: None,
            location: None,
            message: "1 task blocked".into(),
        };
        let text = failure.to_string();
        assert!(
            text.starts_with("simjig Deadlock at t=5 ns: 1 task blocked"),
            "{text}"
        );
        assert!(text.contains("task `main.a` awaits Lock `bus`"), "{text}");
        let help = failure.help().unwrap();
        assert!(
            help.starts_with("each listed task waits on another"),
            "{help}"
        );
        assert!(text.ends_with(&format!("\n  help: {help}")), "{text}");
    }
}
