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
//! - every FST file the replay wrote, each with its `GTKWave` save file (`<stem>.gtkw`, the
//!   marker at the failure): `waves.fst` for the first model, `waves.model-<i>.fst` for the
//!   others, and `waves[.model-<i>].branch-<j>.fst` for each `fork` branch;
//! - `replay/` (the replay's `summary.json` and `events.jsonl`, to compare with this run's).
//!
//! When a fork branch failed the test (`Branches::into_values`), the test fails at the
//! checkpoint's time, but the branch ran on past it: its files are listed first and their
//! marker is at the branch's own failure. The trace is never cut at the checkpoint (a
//! default window runs to the end of every process). A branch whose child died without a
//! report (a signal) has no failure time and an unfinished file (its trace was never
//! closed, so it is often empty): that file is listed last, labelled as incomplete, with no
//! marker, and without a `gtkwave` command when it has no FST header.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::fork::{BranchEnd, FailedBranch};
use crate::{Precision, SimTime};

use super::report::RunIdentity;
use super::rundir;
use super::waves::{RunEnd, Verdict, WavesFile, WavesReplay, WavesRequest, gtkw};

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
    /// The fork branch whose failure failed the test, if one did.
    pub(crate) failed_branch: Option<FailedBranch>,
}

/// A trace file the replay wrote, moved into the run directory: which model and branch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Written {
    fst: PathBuf,
    model: usize,
    branch: Vec<usize>,
}

/// Replays `original` with waves as `request` asks, with the test binary `exe`.
pub(crate) fn replay(exe: &Path, original: &Original<'_>, request: &WavesRequest) -> WavesReplay {
    let (from, to, verdict, written) = match request.span(original.target, original.precision) {
        Err(why) => (SimTime::ZERO, None, Verdict::NotRun(why), Vec::new()),
        Ok((from, to)) => match run_child(exe, original, from, to) {
            Ok((end, written)) => {
                let lines = original.end.differences(&end, original.precision);
                let verdict = if lines.is_empty() {
                    Verdict::Reproduced
                } else {
                    Verdict::NonDeterministic(lines)
                };
                (from, to, verdict, written)
            }
            Err(why) => (from, to, Verdict::NotRun(why), Vec::new()),
        },
    };
    let mut files: Vec<WavesFile> = written
        .into_iter()
        .map(|w| waves_file(original, from, w))
        .collect();
    // Readable files first, the failing branch's leading; a dead branch's file last.
    let order = |f: &WavesFile| (f.died.is_some(), !f.failing_branch);
    files.sort_by(|a, b| (order(a), &a.branch, a.model).cmp(&(order(b), &b.branch, b.model)));
    WavesReplay {
        files,
        from,
        to,
        precision: original.precision,
        verdict,
    }
}

/// The report's view of one written file, with its `GTKWave` save file written next to it.
fn waves_file(original: &Original<'_>, from: SimTime, written: Written) -> WavesFile {
    let failing = original
        .failed_branch
        .filter(|b| written.branch.first() == Some(&b.index));
    let (marker, died) = match failing.map(|b| b.end) {
        Some(BranchEnd::Died(exit)) => (None, Some(exit)),
        Some(BranchEnd::Failed(time)) => (Some(time.max(from)), None),
        None => (Some(original.target.max(from)), None),
    };
    let save = |marker: Option<SimTime>| {
        let path = written.fst.with_extension("gtkw");
        let title = match marker {
            Some(m) => format!(" (marker: t={})", m.display(original.precision)),
            None => String::new(),
        };
        let title = format!(
            "simjig: {} seed={}{title}",
            original.run.test, original.run.seed
        );
        let to = marker.unwrap_or(original.target).max(from);
        let text = gtkw(&written.fst, &title, from, to, marker, &original.signals);
        fs::write(&path, text).ok().map(|()| path)
    };
    WavesFile {
        gtkw: has_fst_header(&written.fst).then(|| save(marker)).flatten(),
        fst: written.fst,
        model: written.model,
        branch: written.branch,
        marker,
        failing_branch: failing.is_some(),
        died,
    }
}

/// Whether `fst` starts with an FST header block (`FST_BL_HDR`, type 0, then its length as
/// a big-endian `u64`, all of it in the file): a viewer can open it. A trace whose process
/// died before closing it may have none (it is empty, or holds only buffered blocks).
fn has_fst_header(fst: &Path) -> bool {
    let Ok(bytes) = fs::read(fst) else {
        return false;
    };
    let length = bytes
        .get(1..9)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .map(u64::from_be_bytes);
    bytes.first() == Some(&0)
        && length.is_some_and(|len| len >= 8 && usize::try_from(len).is_ok_and(|l| l < bytes.len()))
}

/// Runs the child and moves its results into the run directory; returns how it ended and
/// the trace files it wrote.
fn run_child(
    exe: &Path,
    original: &Original<'_>,
    from: SimTime,
    to: Option<SimTime>,
) -> Result<(RunEnd, Vec<Written>), String> {
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
) -> Result<(RunEnd, Vec<Written>), String> {
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
    Ok((end, move_waves(root, original.run_dir)?))
}

/// Moves every trace file the replay wrote into `root` (`waves*.fst`) to `run_dir`,
/// after removing an earlier replay's files there.
fn move_waves(root: &Path, run_dir: &Path) -> Result<Vec<Written>, String> {
    let names = |dir: &Path| -> Vec<String> {
        fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    for stale in names(run_dir) {
        let stem = stale
            .strip_suffix(".fst")
            .or_else(|| stale.strip_suffix(".gtkw"));
        if stem.and_then(trace_identity).is_some() {
            fs::remove_file(run_dir.join(&stale)).ok(); // Best effort; a rename replaces it.
        }
    }
    let mut written = Vec::new();
    for name in names(root) {
        let Some((model, branch)) = name.strip_suffix(".fst").and_then(trace_identity) else {
            continue;
        };
        let dest = run_dir.join(&name);
        fs::rename(root.join(&name), &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        written.push(Written {
            fst: dest,
            model,
            branch,
        });
    }
    Ok(written)
}

/// The model and fork branch path of a trace file stem the replay writes: `waves`, then
/// optionally `.model-<i>` (`ModelOptions::for_model`), then `.branch-<j>` once per
/// checkpoint level (`model::trace::branch_path`). `None` for any other name.
fn trace_identity(stem: &str) -> Option<(usize, Vec<usize>)> {
    let rest = stem.strip_prefix("waves")?;
    let mut model = 0;
    let mut branch = Vec::new();
    for (i, part) in rest.split('.').enumerate() {
        match (i, part) {
            (0, "") => {}
            (0, _) => return None,
            (1, part) if part.starts_with("model-") => {
                model = part.strip_prefix("model-")?.parse().ok()?;
            }
            (_, part) => branch.push(part.strip_prefix("branch-")?.parse().ok()?),
        }
    }
    Some((model, branch))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_file_names_give_the_model_and_branch() {
        assert_eq!(trace_identity("waves"), Some((0, vec![])));
        assert_eq!(trace_identity("waves.model-2"), Some((2, vec![])));
        assert_eq!(trace_identity("waves.branch-3"), Some((0, vec![3])));
        assert_eq!(trace_identity("waves.model-1.branch-0"), Some((1, vec![0])));
        assert_eq!(
            trace_identity("waves.branch-1.branch-4"),
            Some((0, vec![1, 4]))
        );
        for other in [
            "wavesx",
            "waves.model-x",
            "waves.branch-0.model-1",
            "summary",
            "waves.x",
        ] {
            assert_eq!(trace_identity(other), None, "{other}");
        }
    }

    #[test]
    fn only_a_file_starting_with_a_whole_header_block_has_a_header() {
        let dir = std::env::temp_dir().join(format!("simjig-hdr-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut whole = vec![0];
        whole.extend(16u64.to_be_bytes());
        whole.extend([0; 8]);
        for (name, bytes, expect) in [
            ("empty", Vec::new(), false),
            ("whole", whole.clone(), true),
            ("cut", whole[..12].to_vec(), false),
            ("value-changes", [&[1], &whole[1..]].concat(), false),
        ] {
            let path = dir.join(name);
            fs::write(&path, bytes).unwrap();
            assert_eq!(has_fst_header(&path), expect, "{name}");
        }
        assert!(!has_fst_header(&dir.join("missing")));
        fs::remove_dir_all(&dir).ok();
    }
}
