//! The run directory: `<target>/simjig/runs/<test>/<seed>/`, written after every
//! `#[simjig::test]` run, passed or failed.
//!
//! It holds two files, read by people and by later tools (`cargo simjig triage`):
//! - `summary.json`: one object, schema `"simjig.run/1"` (fields in [`Summary`]);
//! - `events.jsonl`: the flight recorder's window, one kernel event per line, oldest first:
//!   `{"t":<ticks>,"kind":"<TraceKind>","task":<id or null>,"arg":<u64>,"value":<u64>}`.
//!   The recorder keeps the most recent 4096 events; `summary.json` says how many were
//!   recorded in total, and its `trace_hash` covers all of them.
//!
//! `<test>` is the libtest name with `::` replaced by `__`; `<seed>` is 16 hex digits.
//! The JSON is written by hand (about 60 lines) so the runtime crate needs no serializer.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{TimeDisplay, TraceRecord};

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

    /// `{"ticks": n, "display": "1250 ns"}`.
    pub(crate) fn time(ticks: u64, display: TimeDisplay) -> Self {
        Self::Obj(vec![
            ("ticks", Self::U64(ticks)),
            ("display", Self::str(display.to_string())),
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

/// The directory for one run under `root` (`<root>/<test>/<seed hex>`).
pub(crate) fn run_dir(root: &Path, test: &str, seed: Seed) -> PathBuf {
    root.join(test.replace("::", "__"))
        .join(format!("{:016x}", seed.value()))
}

/// `<target>/simjig/runs`, where `<target>` is Cargo's target directory: the nearest
/// ancestor of the test binary (`<target>/[<triple>/]<profile>/deps/<bin>`) that holds the
/// `CACHEDIR.TAG` Cargo writes there.
pub(crate) fn default_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let target = exe
        .ancestors()
        .skip(1)
        .take(5)
        .find(|dir| dir.join("CACHEDIR.TAG").is_file())?;
    Some(target.join("simjig").join("runs"))
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
                ("time", Json::time(r.time.0, r.time.display(r.precision))),
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

/// Replaces `dir` with a fresh `summary.json` and, if given, `events.jsonl`.
pub(crate) fn write(
    dir: &Path,
    summary: &Summary<'_>,
    events: Option<&[TraceRecord]>,
) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?; // Our own output from an earlier run of this test and seed.
    }
    fs::create_dir_all(dir)?;
    let mut text = String::new();
    summary.to_json().write(&mut text, Some(0));
    text.push('\n');
    fs::write(dir.join("summary.json"), text)?;
    if let Some(events) = events {
        let mut text = String::new();
        for record in events {
            text.push_str(&event_line(record));
            text.push('\n');
        }
        fs::write(dir.join("events.jsonl"), text)?;
    }
    Ok(())
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

    #[test]
    fn run_dirs_are_named_by_test_and_seed() {
        let seed = Seed::new(0xab, super::super::seed::SeedSource::Env);
        assert_eq!(
            run_dir(Path::new("r"), "m::t", seed),
            Path::new("r/m__t/00000000000000ab")
        );
    }
}
