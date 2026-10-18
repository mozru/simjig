//! The replay behind waves on failure: the same test binary runs the same test again, in a
//! child process, with the same seed and with FST tracing over the window
//! `harness::waves` chose; then its waves and its ending are collected.
//!
//! Why a child process and not a second run in this one: a test body is an `FnOnce`, the
//! models keep process-wide state (Verilator's, the user's statics), and a fresh process is
//! what `SIMJIG_SEED=<seed> cargo nextest run ...` (the repro line) gives too. So the replay
//! is exactly the reproduction a user would run.
//!
//! The child is told what to trace by three variables the harness reads in any run:
//! `SIMJIG_TRACE` (the FST path), `SIMJIG_TRACE_FROM` and `SIMJIG_TRACE_TO` (ticks of the
//! design's precision). It writes its run directory under a private root (a dot-directory
//! next to this run's directory, so it never replaces this run's own), and the parent moves
//! the results into this run's directory:
//! - `waves.fst` and `waves.gtkw` (a `GTKWave` save file with the marker at the failure);
//! - `replay/` (the replay's `summary.json` and `events.jsonl`, to compare with this run's).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Precision, SimTime};

use super::report::RunIdentity;
use super::rundir;
use super::waves::{RunEnd, Verdict, WavesReplay, WavesRequest, gtkw};

/// The variables a replay child reads, and the waves variables it must not see (or it would
/// replay itself).
pub(crate) const TRACE_VAR: &str = "SIMJIG_TRACE";
pub(crate) const TRACE_FROM_VAR: &str = "SIMJIG_TRACE_FROM";
pub(crate) const TRACE_TO_VAR: &str = "SIMJIG_TRACE_TO";

/// The run being replayed.
#[derive(Debug)]
pub(crate) struct Original<'a> {
    pub(crate) run: &'a RunIdentity,
    /// This run's directory, where the waves go.
    pub(crate) run_dir: &'a Path,
    pub(crate) end: RunEnd,
    /// The failure time, or the end of a passing run: where the window ends.
    pub(crate) target: SimTime,
    pub(crate) precision: Precision,
    /// Signals the failure names, shown in the waves view.
    pub(crate) signals: Vec<String>,
}

/// Replays `original` with waves as `request` asks, with the test binary `exe`.
pub(crate) fn replay(exe: &Path, original: &Original<'_>, request: &WavesRequest) -> WavesReplay {
    let (from, to, verdict, fst) = match request.span(original.target, original.precision) {
        Err(why) => (SimTime::ZERO, None, Verdict::NotRun(why), None),
        Ok((from, to)) => match run_child(exe, original, from, to) {
            Ok((end, fst)) => {
                let lines = original.end.differences(&end, original.precision);
                let verdict = if lines.is_empty() {
                    Verdict::Reproduced
                } else {
                    Verdict::NonDeterministic(lines)
                };
                (from, to, verdict, fst)
            }
            Err(why) => (from, to, Verdict::NotRun(why), None),
        },
    };
    let gtkw = fst.as_ref().and_then(|fst| {
        let path = original.run_dir.join("waves.gtkw");
        let title = format!(
            "simjig: {} seed={} (marker: t={})",
            original.run.test,
            original.run.seed,
            super::report::human_time(original.target, original.precision)
        );
        let text = gtkw(fst, &title, from, original.target, &original.signals);
        fs::write(&path, text).ok().map(|()| path)
    });
    WavesReplay {
        fst,
        gtkw,
        from,
        to,
        precision: original.precision,
        verdict,
    }
}

/// Runs the child and moves its results into the run directory; returns how it ended and
/// the waves file, if one was written.
fn run_child(
    exe: &Path,
    original: &Original<'_>,
    from: SimTime,
    to: Option<SimTime>,
) -> Result<(RunEnd, Option<PathBuf>), String> {
    let run = original.run;
    let parent = original
        .run_dir
        .parent()
        .ok_or_else(|| "the run directory has no parent".to_owned())?;
    let root = parent.join(format!(
        ".{:016x}.replay-{}",
        run.seed.value(),
        std::process::id()
    ));
    fs::remove_dir_all(&root).ok(); // A leftover of an earlier, killed process with our pid.
    fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let result = child_in(exe, original, &root, from, to);
    fs::remove_dir_all(&root).ok(); // Best effort: the replay's result matters more.
    result
}

fn child_in(
    exe: &Path,
    original: &Original<'_>,
    root: &Path,
    from: SimTime,
    to: Option<SimTime>,
) -> Result<(RunEnd, Option<PathBuf>), String> {
    let run = original.run;
    let waves = root.join("waves.fst");
    let mut command = Command::new(exe);
    // libtest's own arguments, as nextest passes them: exactly this test, ignored or not.
    command
        .args([
            "--exact",
            &run.test,
            "--include-ignored",
            "--test-threads",
            "1",
        ])
        .env("SIMJIG_SEED", run.seed.to_string())
        .env("SIMJIG_RUNS_DIR", root)
        .env(TRACE_VAR, &waves)
        .env(TRACE_FROM_VAR, from.ticks().to_string())
        .env_remove("SIMJIG_WAVES")
        .env_remove("SIMJIG_WAVES_ON_FAIL");
    match to {
        Some(to) => command.env(TRACE_TO_VAR, to.ticks().to_string()),
        None => command.env_remove(TRACE_TO_VAR),
    };
    let output = command
        .output()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    let binary = run.binary.as_deref().unwrap_or("_");
    let child_dir = rundir::run_dir(root, &run.package, binary, &run.test, run.seed);
    let Ok(summary) = fs::read_to_string(child_dir.join("summary.json")) else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(12).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(format!(
            "the replay ({}) wrote no summary; its last output:\n{}",
            output.status,
            tail.join("\n")
        ));
    };
    let end = parse_summary(&summary)?;
    let replay_dir = original.run_dir.join("replay");
    fs::remove_dir_all(&replay_dir).ok(); // An earlier replay of this run directory.
    fs::rename(&child_dir, &replay_dir).map_err(|e| format!("{}: {e}", replay_dir.display()))?;
    let fst = if waves.is_file() {
        let dest = original.run_dir.join("waves.fst");
        fs::rename(&waves, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        Some(dest)
    } else {
        None
    };
    Ok((end, fst))
}

/// How the run in a `summary.json` (as `rundir` writes it) ended.
pub(crate) fn parse_summary(text: &str) -> Result<RunEnd, String> {
    let bad = || "the replay's summary.json has an unexpected shape".to_owned();
    let failure = match text.split_once("\n  \"failure\": ") {
        Some((_, rest)) if rest.starts_with("null") => None,
        Some((_, rest)) => {
            let kind = string_field(rest, "kind").ok_or_else(bad)?;
            let signature = string_field(rest, "signature").ok_or_else(bad)?;
            let time = number_field(rest, "ticks").ok_or_else(bad)?;
            Some((kind, signature, time))
        }
        None => return Err(bad()),
    };
    let end = text
        .split_once("\"end_time\": ")
        .and_then(|(_, rest)| number_field(rest, "ticks"));
    Ok(RunEnd {
        failure,
        end,
        trace_hash: string_field(text, "trace_hash"),
    })
}

/// The first `"key": "<value>"` in `text`, unescaped only as far as signatures, kinds and
/// hashes need (they hold no escapes).
fn string_field(text: &str, key: &str) -> Option<String> {
    let (_, rest) = text.split_once(&format!("\"{key}\": \""))?;
    rest.split_once('"').map(|(value, _)| value.to_owned())
}

/// The first `"key": <number>` in `text`.
fn number_field(text: &str, key: &str) -> Option<u64> {
    let (_, rest) = text.split_once(&format!("\"{key}\": "))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}
