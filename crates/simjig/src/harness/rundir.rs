//! The run directory: `<target>/simjig/runs/<package>/<binary>/<test>/<seed>/`, written
//! after every `#[simjig::test]` run, passed or failed.
//!
//! It holds two files, read by people and by later tools (`cargo simjig triage`):
//! - `summary.json`: one object, schema `"simjig.run/1"` (fields in [`Summary`]);
//! - `events.jsonl`: the flight recorder's window, one kernel event per line, oldest first:
//!   `{"t":<ticks>,"kind":"<TraceKind>","task":<id or null>,"arg":<u64>,"value":<u64>}`.
//!   The recorder keeps the most recent 4096 events; `summary.json` says how many were
//!   recorded in total, and its `trace_hash` covers all of them.
//!
//! `<package>` is the Cargo package and `<binary>` the test binary (`harness_tests`, as
//! nextest names it), so same-named tests in two test files or packages never share a
//! directory. `<test>` is the libtest name with `::` replaced by `__`; `<seed>` is 16 hex
//! digits.
//!
//! The same test and seed can still run in two processes at once (two `cargo nextest`
//! invocations). So a run is written into a temporary sibling (`.<seed>.tmp-<pid>-<n>`)
//! and renamed into place: a reader sees the previous complete directory, none, or the
//! new complete one, and the last writer wins. A process killed mid-write leaves its
//! temporary sibling behind; names starting with `.` are never run directories.
//! The JSON is written by hand (about 60 lines) so the runtime crate needs no serializer.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Precision, SimTime, TraceRecord};

use super::dut::InitValues;
use super::report::FailureReport;
use super::seed::Seed;

/// A JSON value, enough for the run directory's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Json {
    Null,
    U64(u64),
    Str(String),
    Obj(Vec<(&'static str, Json)>),
}

impl Json {
    pub(crate) fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }

    pub(crate) fn opt_str(s: Option<impl Into<String>>) -> Self {
        s.map_or(Self::Null, Self::str)
    }

    /// `{"ticks": n, "display": "1250 ns"}`, displayed as reports print times.
    pub(crate) fn time(time: SimTime, precision: Precision) -> Self {
        Self::Obj(vec![
            ("ticks", Self::U64(time.0)),
            ("display", Self::str(time.display(precision).to_string())),
        ])
    }

    /// Compact (`indent` = `None`) or pretty-printed with two-space indentation.
    pub(crate) fn write(&self, out: &mut String, indent: Option<usize>) {
        match self {
            Self::Null => out.push_str("null"),
            Self::U64(v) => out.push_str(&v.to_string()),
            Self::Str(s) => write_str(out, s),
            Self::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if let Some(depth) = indent {
                        out.push('\n');
                        out.push_str(&"  ".repeat(depth + 1));
                    }
                    write_str(out, key);
                    out.push(':');
                    if indent.is_some() {
                        out.push(' ');
                    }
                    value.write(out, indent.map(|d| d + 1));
                }
                if let Some(depth) = indent.filter(|_| !fields.is_empty()) {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
                out.push('}');
            }
        }
    }
}

/// A JSON string literal (RFC 8259 §7).
fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let b = c as usize;
                out.push_str("\\u00");
                out.push(char::from(HEX[b >> 4]));
                out.push(char::from(HEX[b & 0xf]));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// One kernel event as an `events.jsonl` line (without the newline).
pub(crate) fn event_line(record: &TraceRecord) -> String {
    let task = if record.task == u64::MAX {
        Json::Null
    } else {
        Json::U64(record.task)
    };
    let mut out = String::new();
    Json::Obj(vec![
        ("t", Json::U64(record.time.0)),
        ("kind", Json::str(format!("{:?}", record.kind))),
        ("task", task),
        ("arg", Json::U64(record.arg)),
        ("value", Json::U64(record.value)),
    ])
    .write(&mut out, None);
    out
}

/// The directory for one run under `root`
/// (`<root>/<package>/<binary>/<test>/<seed hex>`).
pub(crate) fn run_dir(root: &Path, package: &str, binary: &str, test: &str, seed: Seed) -> PathBuf {
    root.join(package)
        .join(binary)
        .join(test.replace("::", "__"))
        .join(format!("{:016x}", seed.value()))
}

/// The name of the running test binary as nextest's `binary()` filter matches it: the
/// executable's name without Cargo's `-<16 hex digits>` suffix (`harness_tests`).
pub(crate) fn binary_name(exe: &Path) -> Option<String> {
    let stem = exe.file_stem()?.to_str()?;
    let name = match stem.rsplit_once('-') {
        Some((name, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            name
        }
        _ => stem,
    };
    Some(name.to_owned())
}

/// `<target>/simjig/runs`, where `<target>` is Cargo's target directory
/// ([`target_dir_of`] the running test binary).
pub(crate) fn default_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    let target = target_dir_of(
        &exe,
        env!("SIMJIG_TARGET_TRIPLE"),
        cargo_target_dir.as_deref(),
    )?;
    Some(target.join("simjig").join("runs"))
}

/// The Cargo target directory of the test binary `exe`, found as simjig-build finds the
/// build cache's (`BuildEnv::cache_dir`): the nearest ancestor (of the five a binary in
/// `<target>/[<triple>/]<profile>/deps/<bin>` has) holding the `CACHEDIR.TAG` Cargo writes
/// at the root of a target directory it creates; else an absolute `CARGO_TARGET_DIR` that
/// holds `exe`; else the directory Cargo's layout names, with the `<triple>/` level when
/// its name is `triple`, the target simjig was built for.
///
/// The tag alone is not enough: Cargo writes it only when it creates the directory, so a
/// `target/` another tool made first (`--target-dir target/rust-analyzer` creates it on
/// its way) never gets one.
fn target_dir_of(exe: &Path, triple: &str, cargo_target_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(tagged) = exe
        .ancestors()
        .skip(1)
        .take(5)
        .find(|dir| dir.join("CACHEDIR.TAG").is_file())
    {
        return Some(tagged.to_path_buf());
    }
    if let Some(dir) = cargo_target_dir.filter(|d| d.is_absolute() && exe.starts_with(d)) {
        return Some(dir.to_path_buf());
    }
    let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
    let deps = exe
        .parent()
        .filter(|d| name(d).as_deref() == Some("deps"))?;
    let above = deps.parent()?.parent()?;
    match above.parent() {
        Some(target) if name(above).as_deref() == Some(triple) => Some(target.to_path_buf()),
        _ => Some(above.to_path_buf()),
    }
}

/// The contents of `summary.json`, before rendering.
#[derive(Debug)]
pub(crate) struct Summary<'a> {
    pub(crate) test: &'a str,
    pub(crate) package: &'a str,
    /// What the repro command runs `cargo` through, if anything (`pixi run`).
    pub(crate) launcher: Option<&'a str>,
    pub(crate) seed: Seed,
    pub(crate) verilator_seed: u32,
    pub(crate) init: InitValues,
    /// Model 0's [`Seed::init_seed`] with random initial values, else `None`.
    pub(crate) init_seed: Option<u32>,
    /// `None` if the model could not be built.
    pub(crate) precision: Option<String>,
    pub(crate) end_time: Json,
    pub(crate) timeout: Json,
    pub(crate) trace_hash: Option<u64>,
    /// Events written to `events.jsonl`, and recorded in total.
    pub(crate) events: Option<(usize, u64)>,
    pub(crate) failure: Option<&'a FailureReport>,
}

impl Summary<'_> {
    pub(crate) fn to_json(&self) -> Json {
        let failure = self.failure.map_or(Json::Null, |r| {
            Json::Obj(vec![
                ("kind", Json::str(r.kind)),
                ("signature", Json::str(r.signature.to_string())),
                ("time", Json::time(r.time, r.precision)),
                ("task", Json::opt_str(r.task.clone())),
                ("location", Json::opt_str(r.location.clone())),
                ("message", Json::str(r.message.clone())),
                ("repro", Json::str(r.run.repro())),
                ("waves", Json::str(r.run.waves())),
                ("report", Json::str(r.to_string())),
            ])
        });
        let events = self.events.map_or(Json::Null, |(written, recorded)| {
            Json::Obj(vec![
                ("file", Json::str("events.jsonl")),
                ("written", Json::U64(written as u64)),
                ("recorded", Json::U64(recorded)),
            ])
        });
        Json::Obj(vec![
            ("schema", Json::str("simjig.run/1")),
            ("test", Json::str(self.test)),
            ("package", Json::str(self.package)),
            ("launcher", Json::opt_str(self.launcher)),
            (
                "status",
                Json::str(if self.failure.is_some() {
                    "fail"
                } else {
                    "pass"
                }),
            ),
            ("seed", Json::str(self.seed.to_string())),
            ("seed_source", Json::str(self.seed.source().as_str())),
            ("verilator_seed", Json::U64(u64::from(self.verilator_seed))),
            ("init", Json::str(self.init.as_str())),
            (
                "init_seed",
                self.init_seed
                    .map_or(Json::Null, |s| Json::U64(u64::from(s))),
            ),
            ("precision", Json::opt_str(self.precision.clone())),
            ("end_time", self.end_time.clone()),
            ("timeout", self.timeout.clone()),
            (
                "trace_hash",
                Json::opt_str(self.trace_hash.map(|h| format!("{h:#018x}"))),
            ),
            ("events", events),
            ("failure", failure),
        ])
    }
}

/// Replaces `dir` with a fresh `summary.json` and, if given, `events.jsonl`, atomically
/// (see the module docs).
pub(crate) fn write(
    dir: &Path,
    summary: &Summary<'_>,
    events: Option<&[TraceRecord]>,
) -> io::Result<()> {
    let (parent, name) = match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(name)) => (parent, name.to_string_lossy()),
        _ => return Err(io::Error::other("not a run directory path")),
    };
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{name}.tmp-{}", unique()));
    let written = fill(&tmp, summary, events).and_then(|()| replace(&tmp, dir));
    if written.is_err() {
        fs::remove_dir_all(&tmp).ok(); // Best effort: the error being returned matters more.
    }
    written
}

/// `<pid>-<n>`: unique among concurrent writers on this machine.
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn fill(tmp: &Path, summary: &Summary<'_>, events: Option<&[TraceRecord]>) -> io::Result<()> {
    fs::create_dir(tmp)?;
    let mut text = String::new();
    summary.to_json().write(&mut text, Some(0));
    text.push('\n');
    fs::write(tmp.join("summary.json"), text)?;
    if let Some(events) = events {
        let mut text = String::new();
        for record in events {
            text.push_str(&event_line(record));
            text.push('\n');
        }
        fs::write(tmp.join("events.jsonl"), text)?;
    }
    Ok(())
}

/// Renames `tmp` to `dir`. `rename` cannot replace a non-empty directory, so an existing
/// `dir` (an earlier run, or a concurrent writer that got there first) is renamed aside,
/// and deleted once `tmp` is in place. Another writer can move `dir` between our two
/// renames, so a failed rename is retried.
fn replace(tmp: &Path, dir: &Path) -> io::Result<()> {
    const ATTEMPTS: usize = 16;
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let mut aside = Vec::new();
    let mut result = Err(io::Error::other("the run directory kept changing"));
    for _ in 0..ATTEMPTS {
        match fs::rename(tmp, dir) {
            Ok(()) => {
                result = Ok(());
                break;
            }
            // POSIX `rename` reports a non-empty target as ENOTEMPTY or EEXIST.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists
                ) || dir.is_dir() => {}
            Err(e) => {
                result = Err(e);
                break;
            }
        }
        let old = dir.with_file_name(format!(".{name}.old-{}", unique()));
        match fs::rename(dir, &old) {
            Ok(()) => aside.push(old),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {} // Another writer moved it.
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    for old in aside {
        fs::remove_dir_all(old).ok(); // Best effort: a leftover is named `.<seed>.old-*`.
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_strings() {
        let mut out = String::new();
        Json::str("a\"b\\c\nd\u{1}é").write(&mut out, None);
        assert_eq!(out, r#""a\"b\\c\nd\u0001é""#);
    }

    #[test]
    fn json_pretty_prints_nested_objects() {
        let value = Json::Obj(vec![
            ("a", Json::U64(1)),
            ("b", Json::Obj(vec![("c", Json::Null)])),
            ("d", Json::Obj(vec![])),
        ]);
        let mut out = String::new();
        value.write(&mut out, Some(0));
        assert_eq!(
            out,
            "{\n  \"a\": 1,\n  \"b\": {\n    \"c\": null\n  },\n  \"d\": {}\n}"
        );
    }

    /// A target directory without Cargo's `CACHEDIR.TAG` is still found: by an absolute
    /// `CARGO_TARGET_DIR` that holds the binary, else by Cargo's layout, with or without
    /// the `<triple>/` level; a tag wins over both.
    #[test]
    fn the_target_dir_is_found_without_its_cachedir_tag() {
        let root = std::env::temp_dir().join(format!("simjig-target-dir-{}", std::process::id()));
        let target = root.join("target");
        let plain = target.join("debug/deps/harness_tests-0123456789abcdef");
        let cross = target.join("aarch64-apple-darwin/release/deps/t-0123456789abcdef");
        let found =
            |exe: &Path, ctd: Option<&Path>| target_dir_of(exe, "aarch64-apple-darwin", ctd);
        assert_eq!(found(&plain, None), Some(target.clone()));
        assert_eq!(found(&cross, None), Some(target.clone()));
        // Another triple's directory is not this build's `<triple>/` level.
        assert_eq!(
            target_dir_of(&cross, "x86_64-unknown-linux-gnu", None),
            Some(target.join("aarch64-apple-darwin"))
        );
        assert_eq!(found(&plain, Some(&target)), Some(target.clone()));
        // A relative one, or one that does not hold the binary, is not it.
        assert_eq!(
            found(&plain, Some(Path::new("target"))),
            Some(target.clone())
        );
        let elsewhere = root.join("elsewhere");
        assert_eq!(found(&plain, Some(&elsewhere)), Some(target.clone()));
        // Not a binary in `deps/`, no tag: no target directory.
        assert_eq!(found(&root.join("bin/tool"), None), None);
        // A tag wins (a nested target directory, `target/rust-analyzer`).
        let nested = target.join("ra");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("CACHEDIR.TAG"), "Signature: x").unwrap();
        let in_nested = nested.join("debug/deps/t-0123456789abcdef");
        let result = found(&in_nested, Some(&target));
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(result, Some(nested));
    }

    #[test]
    fn run_dirs_are_named_by_test_and_seed() {
        let seed = Seed::new(0xab, super::super::seed::SeedSource::Env);
        assert_eq!(
            run_dir(Path::new("r"), "p", "b", "m::t", seed),
            Path::new("r/p/b/m__t/00000000000000ab")
        );
    }

    #[test]
    fn binary_names_drop_cargo_hashes() {
        let name = |p: &str| binary_name(Path::new(p));
        assert_eq!(
            name("t/debug/deps/harness_tests-b88c319aa24eca27").as_deref(),
            Some("harness_tests")
        );
        assert_eq!(
            name("t/debug/deps/simjig_bits-5ff542379917abd3.exe").as_deref(),
            Some("simjig_bits")
        );
        assert_eq!(name("t/debug/my-tool").as_deref(), Some("my-tool"));
    }
}
