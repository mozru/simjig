//! Waves on failure: what `SIMJIG_WAVES` asks for, which window the replay traces, how the
//! replay is judged, and the files and lines it produces. The replay itself (a child
//! process) is `harness::replay`.
//!
//! A run is never traced while it is first run: tracing costs time on every run, and most
//! runs pass. When waves are asked for, the harness runs the same test again with the same
//! seed, tracing from `t_fail - window`, and checks that the replay ends the same way
//! (DECISION §1: "waves from a deterministic replay; the replay doubles as the determinism
//! check"). A replay that differs is reported as non-determinism, never retried.
//!
//! # Environment
//! - `SIMJIG_WAVES=1`: after the run (passed or failed), replay it with waves over the
//!   default window: the last 10% of the run, or 1 us, whichever is longer, ending at the
//!   failure (or the end of a passing run).
//! - `SIMJIG_WAVES=window:<t0>..<t1>`: the same, with waves from `t0` to `t1` (`2us`,
//!   `1500ns`, `0`; either end may be left out).
//! - `SIMJIG_WAVES_ON_FAIL=1`: replay with the default window after a failure only (meant to
//!   stay on in CI). `SIMJIG_WAVES` wins when both are set.
//! - `0` or an empty value turns either off.

use std::fmt;
use std::path::PathBuf;

use crate::{FailureKind, Precision, SimDuration, SimTime, TimeUnit};

/// When the harness replays a run with waves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavesWhen {
    /// After every run (`SIMJIG_WAVES`).
    Always,
    /// After a failed run only (`SIMJIG_WAVES_ON_FAIL`).
    OnFailure,
}

/// One end of an explicit window: an amount of a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowEnd {
    /// The amount.
    pub value: u64,
    /// Its unit.
    pub unit: TimeUnit,
}

/// The part of the run the replay traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// The last 10% of the run, or 1 us, whichever is longer.
    Default,
    /// From `from` (or 0) to `to` (or the end).
    Explicit {
        /// The first time traced.
        from: Option<WindowEnd>,
        /// The last time traced.
        to: Option<WindowEnd>,
    },
}

/// What the environment asks the harness for; see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavesRequest {
    /// When to replay.
    pub when: WavesWhen,
    /// What to trace.
    pub window: Window,
}

impl WavesRequest {
    /// The request made by `SIMJIG_WAVES` (`waves`) and `SIMJIG_WAVES_ON_FAIL` (`on_fail`),
    /// or `None` for no waves.
    ///
    /// # Errors
    /// A message naming the variable and the accepted forms, for any other value.
    pub fn from_vars(waves: Option<&str>, on_fail: Option<&str>) -> Result<Option<Self>, String> {
        let off = |v: &str| v.trim().is_empty() || v.trim() == "0";
        if let Some(text) = waves.filter(|v| !off(v)) {
            let window = match text.trim() {
                "1" => Window::Default,
                other => match other.strip_prefix("window:") {
                    Some(range) => parse_window(range)
                        .map_err(|e| format!("SIMJIG_WAVES={text:?}: {e}; {WAVES_FORMS}"))?,
                    None => {
                        return Err(format!("SIMJIG_WAVES={text:?} is not valid; {WAVES_FORMS}"));
                    }
                },
            };
            return Ok(Some(Self {
                when: WavesWhen::Always,
                window,
            }));
        }
        match on_fail.filter(|v| !off(v)).map(str::trim) {
            None => Ok(None),
            Some("1") => Ok(Some(Self {
                when: WavesWhen::OnFailure,
                window: Window::Default,
            })),
            Some(other) => Err(format!(
                "SIMJIG_WAVES_ON_FAIL={other:?} is not valid; use 1 (or 0 for off)"
            )),
        }
    }

    /// Whether a run that `failed` (or not) is replayed.
    #[must_use]
    pub fn applies(&self, failed: bool) -> bool {
        failed || self.when == WavesWhen::Always
    }

    /// The traced span for a run that ended (failed or passed) at `target`, in ticks of
    /// `precision`: `(from, to)`, `to = None` meaning the end of the replay.
    ///
    /// # Errors
    /// If an explicit end is not a whole number of ticks, or `from` is after `to`.
    pub fn span(
        &self,
        target: SimTime,
        precision: Precision,
    ) -> Result<(SimTime, Option<SimTime>), String> {
        match self.window {
            Window::Default => {
                // Below the precision, 1 us is less than a tick: 10% decides alone.
                let one_us = precision
                    .duration(1, TimeUnit::Us)
                    .map_or(0, SimDuration::ticks);
                let span = (target.ticks() / 10).max(one_us);
                Ok((SimTime(target.ticks().saturating_sub(span)), None))
            }
            Window::Explicit { from, to } => {
                let ticks = |end: Option<WindowEnd>| {
                    end.map(|e| {
                        precision
                            .duration(e.value, e.unit)
                            .map(|d| SimTime(d.ticks()))
                            .map_err(|err| format!("SIMJIG_WAVES window: {err}"))
                    })
                    .transpose()
                };
                let from = ticks(from)?.unwrap_or(SimTime::ZERO);
                let to = ticks(to)?;
                if to.is_some_and(|to| to < from) {
                    return Err("SIMJIG_WAVES window: it ends before it starts".to_owned());
                }
                Ok((from, to))
            }
        }
    }
}

const WAVES_FORMS: &str = "use SIMJIG_WAVES=1, or SIMJIG_WAVES=window:<t0>..<t1> with times \
                           such as 0, 1500ns or 2us (either end may be left out)";

/// `<t0>..<t1>`, either side optional.
fn parse_window(text: &str) -> Result<Window, String> {
    let (from, to) = text
        .split_once("..")
        .ok_or_else(|| "a window is <t0>..<t1>".to_owned())?;
    let end = |s: &str| {
        let s = s.trim();
        if s.is_empty() {
            Ok(None)
        } else {
            parse_time(s).map(Some)
        }
    };
    Ok(Window::Explicit {
        from: end(from)?,
        to: end(to)?,
    })
}

/// `<integer><unit>` (`_` separators and a space allowed), or a bare `0`.
fn parse_time(text: &str) -> Result<WindowEnd, String> {
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '_'))
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let value: u64 = digits
        .replace('_', "")
        .parse()
        .map_err(|_bad| format!("{text:?} is not a time"))?;
    let unit = match unit.trim() {
        "fs" => TimeUnit::Fs,
        "ps" => TimeUnit::Ps,
        "ns" => TimeUnit::Ns,
        "us" => TimeUnit::Us,
        "ms" => TimeUnit::Ms,
        "s" => TimeUnit::S,
        "" if value == 0 => TimeUnit::S,
        other => {
            return Err(format!(
                "{other:?} in {text:?} is not a unit (fs ps ns us ms s)"
            ));
        }
    };
    Ok(WindowEnd { value, unit })
}

/// How a run ended, as far as the determinism check compares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunEnd {
    /// The failure kind, signature and time in ticks; `None` for a pass.
    pub(crate) failure: Option<(String, String, u64)>,
    /// The time the run ended, in ticks.
    pub(crate) end: Option<u64>,
    /// The kernel's trace hash, `0x`-prefixed.
    pub(crate) trace_hash: Option<String>,
}

impl RunEnd {
    /// What differs between this run and its `replay`, one line each; empty if nothing.
    #[must_use]
    pub(crate) fn differences(&self, replay: &Self, precision: Precision) -> Vec<String> {
        let time = |t: u64| SimTime(t).display(precision);
        let mut out = Vec::new();
        match (&self.failure, &replay.failure) {
            (Some((kind, sig, t)), Some((rkind, rsig, rt))) => {
                if kind != rkind || sig != rsig {
                    out.push(format!(
                        "failure:  {kind} sig={sig} in this run, {rkind} sig={rsig} in the replay"
                    ));
                }
                if t != rt {
                    out.push(format!(
                        "time:     t={} in this run, t={} in the replay",
                        time(*t),
                        time(*rt)
                    ));
                }
            }
            (Some((kind, _, t)), None) => out.push(format!(
                "failure:  {kind} at t={} in this run; the replay passed",
                time(*t)
            )),
            (None, Some((kind, _, t))) => out.push(format!(
                "failure:  this run passed; the replay failed with {kind} at t={}",
                time(*t)
            )),
            (None, None) => {
                if let (Some(a), Some(b)) = (self.end, replay.end)
                    && a != b
                {
                    out.push(format!(
                        "end:      t={} in this run, t={} in the replay",
                        time(a),
                        time(b)
                    ));
                }
            }
        }
        if out.is_empty()
            && let (Some(a), Some(b)) = (&self.trace_hash, &replay.trace_hash)
            && a != b
        {
            out.push(format!(
                "events:   trace hash {a} in this run, {b} in the replay (the same ending, \
                 reached through different kernel events)"
            ));
        }
        out
    }
}

/// What a replay with waves produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WavesReplay {
    /// Every FST file the replay wrote, moved into the run directory: one per model
    /// (`waves.fst`, `waves.model-<i>.fst`) and per fork branch of each
    /// (`waves[.model-<i>].branch-<j>.fst`). The files of a failing fork branch come first,
    /// then the rest in (branch, model) order; empty if no model could trace.
    pub files: Vec<WavesFile>,
    /// The traced span: from, and to (`None`: the end of the run).
    pub from: SimTime,
    /// The last time traced, if not the end.
    pub to: Option<SimTime>,
    /// The design's precision, to print times.
    pub precision: Precision,
    /// The verdict.
    pub verdict: Verdict,
}

/// One FST file of a replay, and its `GTKWave` save file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WavesFile {
    /// The FST file, in the run directory.
    pub fst: PathBuf,
    /// The `GTKWave` save file for it (`<stem>.gtkw`), if it could be written.
    pub gtkw: Option<PathBuf>,
    /// The model it traces (0 for the first DUT).
    pub model: usize,
    /// The fork branch it traces: empty for the test's own process, `[j]` for branch `j`
    /// of a checkpoint, `[j, k]` for branch `k` of a checkpoint inside branch `j`.
    pub branch: Vec<usize>,
    /// Where the save file's marker is: the failure (the failing branch's own failure, in
    /// its files), or the end of a passing run.
    pub marker: SimTime,
    /// Whether this is a file of the fork branch whose failure failed the test.
    pub failing_branch: bool,
}

impl WavesFile {
    /// The file's model and branch, `None` for model 0 outside any branch.
    fn label(&self, precision: Precision) -> Option<String> {
        let mut parts = Vec::new();
        if self.model > 0 {
            parts.push(format!("model {}", self.model));
        }
        if !self.branch.is_empty() {
            let path: Vec<String> = self.branch.iter().map(ToString::to_string).collect();
            parts.push(format!("fork branch {}", path.join(".")));
        }
        if self.failing_branch {
            parts.push(format!("failed at t={}", self.marker.display(precision)));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// `<fst> (<what>); open with: gtkwave <gtkw>`.
    fn line(&self, what: &str) -> String {
        match &self.gtkw {
            Some(gtkw) => format!(
                "{} ({what}); open with: gtkwave {}",
                self.fst.display(),
                gtkw.display()
            ),
            None => format!("{} ({what})", self.fst.display()),
        }
    }
}

/// Whether the replay reproduced the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Same ending (failure signature and time, or pass and end time) and same trace hash.
    Reproduced,
    /// The replay ended differently: one line per difference.
    NonDeterministic(Vec<String>),
    /// The replay could not be run or read; why.
    NotRun(String),
}

impl WavesReplay {
    /// Whether the replay matched the run.
    #[must_use]
    pub fn reproduced(&self) -> bool {
        self.verdict == Verdict::Reproduced
    }
}

const NONDETERMINISM_HELP: &str = "a simjig test must depend only on its seed; look for wall-clock \
     time, thread scheduling, HashMap or pointer order, files or environment variables read by \
     the test or a model. Compare events.jsonl with replay/events.jsonl in the run directory";

/// The `waves:` and `replay:` lines of a report (two-space indent, labels padded to 10).
impl fmt::Display for WavesReplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let span = match self.to {
            Some(to) => format!(
                "t={} to t={}",
                self.from.display(self.precision),
                to.display(self.precision)
            ),
            None => format!("from t={}", self.from.display(self.precision)),
        };
        let mut files = self.files.iter();
        match files.next() {
            Some(first) => {
                let what = match first.label(self.precision) {
                    Some(label) => format!("{label}; {span}"),
                    None => span,
                };
                write!(f, "  waves:    {}", first.line(&what))?;
            }
            None => write!(
                f,
                "  waves:    none written: the DUT's model cannot trace (a FakeModel, or a \
                 Verilator model built without `.trace_fst(true)`)"
            )?,
        }
        for file in files {
            let what = file
                .label(self.precision)
                .unwrap_or_else(|| "model 0".to_owned());
            write!(f, "\n            {}", file.line(&what))?;
        }
        match &self.verdict {
            Verdict::Reproduced => write!(
                f,
                "\n  replay:   reproduced (same seed, same ending at the same time, same events)"
            ),
            Verdict::NonDeterministic(lines) => {
                write!(
                    f,
                    "\n  replay:   NON-DETERMINISTIC: the replay with the same seed did not \
                     reproduce this run"
                )?;
                for line in lines {
                    write!(f, "\n              {line}")?;
                }
                write!(f, "\n            help: {NONDETERMINISM_HELP}")
            }
            Verdict::NotRun(why) => write!(
                f,
                "\n  replay:   NOT VERIFIED: the replay did not complete: {}",
                why.replace('\n', "\n              ")
            ),
        }
    }
}

/// The signals a failure names, if its kind names any (for the waves view).
#[must_use]
pub(crate) fn failing_signals(kind: &FailureKind) -> Vec<String> {
    match kind {
        FailureKind::WriteConflict { signal, .. } | FailureKind::WriteInReadOnly { signal } => {
            vec![signal.clone()]
        }
        FailureKind::DeltaLimit { signals, .. } => signals.clone(),
        FailureKind::TaskPanic { .. }
        | FailureKind::ModelFatal { .. }
        | FailureKind::DesignFinished
        | FailureKind::Starved { .. }
        | FailureKind::Deadlock { .. }
        | FailureKind::TimeLimit { .. }
        | FailureKind::ReadWriteLimit { .. }
        | FailureKind::DpiPanic { .. } => Vec::new(),
    }
}

/// A `GTKWave` save file for `fst`: the view starts at `from` and spans `from..marker` (at
/// least a tick), the primary marker is at `marker` (the failure), and `signals` (kernel
/// names such as `counter.count`) are shown under Verilator's `TOP` scope.
///
/// Save-file times are in the dump's own ticks. `GTKWave`'s zoom is the base-2 logarithm of
/// pixels per tick; the view is sized to about 1000 pixels.
#[must_use]
pub(crate) fn gtkw(
    fst: &std::path::Path,
    title: &str,
    from: SimTime,
    marker: SimTime,
    signals: &[String],
) -> String {
    let span = marker.ticks().saturating_sub(from.ticks()).max(1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a zoom level only needs a few significant digits"
    )]
    let zoom = (1000.0 / span as f64).log2();
    let mut out = format!(
        "[*] {title}\n[dumpfile] \"{}\"\n[timestart] {}\n*{zoom:.6} {} {}\n",
        fst.display(),
        from.ticks(),
        marker.ticks(),
        ["-1"; 26].join(" ")
    );
    for signal in signals {
        out.push_str("@28\nTOP.");
        out.push_str(signal);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(waves: Option<&str>, on_fail: Option<&str>) -> Result<Option<WavesRequest>, String> {
        WavesRequest::from_vars(waves, on_fail)
    }

    #[test]
    fn the_variables_parse_into_requests() {
        assert_eq!(request(None, None), Ok(None));
        assert_eq!(request(Some("0"), Some("")), Ok(None));
        let always = request(Some("1"), None).unwrap().unwrap();
        assert_eq!(
            (always.when, always.window),
            (WavesWhen::Always, Window::Default)
        );
        assert!(always.applies(false));
        let on_fail = request(None, Some("1")).unwrap().unwrap();
        assert_eq!(on_fail.when, WavesWhen::OnFailure);
        assert!(on_fail.applies(true) && !on_fail.applies(false));
        let window = request(Some("window:1_500ns..2 us"), Some("1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            window.window,
            Window::Explicit {
                from: Some(WindowEnd {
                    value: 1500,
                    unit: TimeUnit::Ns
                }),
                to: Some(WindowEnd {
                    value: 2,
                    unit: TimeUnit::Us
                }),
            }
        );
        let open = request(Some("window:..0"), None).unwrap().unwrap();
        assert!(matches!(
            open.window,
            Window::Explicit {
                from: None,
                to: Some(_)
            }
        ));
        for bad in ["yes", "window:3", "window:1xs..", "window:ns.."] {
            let err = request(Some(bad), None).unwrap_err();
            assert!(err.contains("SIMJIG_WAVES"), "{bad}: {err}");
        }
        assert!(request(None, Some("true")).is_err());
    }

    #[test]
    fn the_default_window_is_a_tenth_of_the_run_or_one_microsecond() {
        let default = WavesRequest {
            when: WavesWhen::Always,
            window: Window::Default,
        };
        let ps = Precision::PS;
        // 100 us run: 10 us.
        assert_eq!(
            default.span(SimTime(100_000_000), ps),
            Ok((SimTime(90_000_000), None))
        );
        // 3 us run: 1 us is longer than 300 ns.
        assert_eq!(
            default.span(SimTime(3_000_000), ps),
            Ok((SimTime(2_000_000), None))
        );
        // Shorter than 1 us: from 0.
        assert_eq!(
            default.span(SimTime(400_000), ps),
            Ok((SimTime::ZERO, None))
        );
        let ms = Precision::from_exponent(-3).unwrap();
        assert_eq!(default.span(SimTime(50), ms), Ok((SimTime(45), None)));
    }

    #[test]
    fn explicit_windows_convert_to_ticks() {
        let w = request(Some("window:2us..3us"), None).unwrap().unwrap();
        assert_eq!(
            w.span(SimTime(0), Precision::NS),
            Ok((SimTime(2000), Some(SimTime(3000))))
        );
        let fine = request(Some("window:1500fs.."), None).unwrap().unwrap();
        assert!(fine.span(SimTime(0), Precision::PS).is_err());
        let backwards = request(Some("window:3us..2us"), None).unwrap().unwrap();
        assert!(backwards.span(SimTime(0), Precision::NS).is_err());
    }

    fn end(failure: Option<(&str, &str, u64)>, hash: &str) -> RunEnd {
        RunEnd {
            failure: failure.map(|(k, s, t)| (k.to_owned(), s.to_owned(), t)),
            end: Some(9),
            trace_hash: Some(hash.to_owned()),
        }
    }

    #[test]
    fn differences_name_what_the_replay_changed() {
        let ns = Precision::NS;
        let run = end(Some(("TaskPanic", "aa", 100)), "0x1");
        assert_eq!(run.differences(&run.clone(), ns), Vec::<String>::new());
        let later = end(Some(("TaskPanic", "aa", 340)), "0x2");
        assert_eq!(
            run.differences(&later, ns),
            ["time:     t=100 ns in this run, t=340 ns in the replay"]
        );
        let passed = end(None, "0x1");
        assert_eq!(run.differences(&passed, ns).len(), 1);
        let same_end_other_events = end(Some(("TaskPanic", "aa", 100)), "0x3");
        assert!(run.differences(&same_end_other_events, ns)[0].starts_with("events:"));
    }

    #[test]
    fn the_save_file_marks_the_failure_and_names_the_signals() {
        let text = gtkw(
            std::path::Path::new("/r/waves.fst"),
            "t",
            SimTime(9_000),
            SimTime(10_000),
            &["counter.en".to_owned()],
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "[dumpfile] \"/r/waves.fst\"");
        assert_eq!(lines[2], "[timestart] 9000");
        let marker: Vec<&str> = lines[3].split(' ').collect();
        assert_eq!(
            (marker[0], marker[1], marker.len()),
            ("*0.000000", "10000", 28)
        );
        assert_eq!(&lines[4..], ["@28", "TOP.counter.en"]);
    }
}
