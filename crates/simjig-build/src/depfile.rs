//! Parser for the Make-style dependency file Verilator writes with `--MMD`
//! (`<obj>/V<prefix>__ver.d`).
//!
//! The file has one rule, `targets : prerequisites`, possibly split over lines with a
//! trailing `\`. The prerequisites are every file Verilator read: the sources, every
//! `` `include ``d file, its own `verilated_std.sv`, and the `verilator_bin` executable.
//! They drive both `cargo::rerun-if-changed` and the cache key.

use std::path::{Path, PathBuf};

use crate::Error;

/// Returns the prerequisites of the single rule in `text`, deduplicated, in file order.
pub(crate) fn parse(path: &Path, text: &str) -> Result<Vec<PathBuf>, Error> {
    let joined = text.replace("\\\n", " ");
    let err = |line: usize, msg: &str| Error::Parse {
        path: path.to_path_buf(),
        line,
        msg: msg.to_owned(),
    };
    let mut rules = joined.lines().filter(|l| !l.trim().is_empty());
    let rule = rules
        .next()
        .ok_or_else(|| err(0, "empty dependency file"))?;
    if rules.next().is_some() {
        return Err(err(
            2,
            "expected exactly one `targets : prerequisites` rule",
        ));
    }
    // Verilator writes ` : ` with spaces; plain `:` could be part of a path.
    let (_, prereqs) = rule
        .split_once(" : ")
        .ok_or_else(|| err(1, "missing ` : ` between targets and prerequisites"))?;
    let mut deps: Vec<PathBuf> = Vec::new();
    for word in split_words(prereqs) {
        let p = PathBuf::from(word);
        if !deps.contains(&p) {
            deps.push(p);
        }
    }
    Ok(deps)
}

/// Splits on unescaped whitespace; `\ ` is a literal space inside a path.
fn split_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                cur.push(' ');
                chars.next();
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deps(text: &str) -> Vec<String> {
        parse(Path::new("x.d"), text)
            .unwrap()
            .into_iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    #[test]
    fn verilator_shape() {
        // Shape copied from a real Verilator 5.052 `--MMD` output.
        let text = "/o/V.cpp /o/V.h  : /p/bin/verilator_bin /p/bin/verilator_bin \
                    /p/include/verilated_std.sv /src/counter.sv /src/defs.svh \n";
        assert_eq!(
            deps(text),
            [
                "/p/bin/verilator_bin",
                "/p/include/verilated_std.sv",
                "/src/counter.sv",
                "/src/defs.svh"
            ]
        );
    }

    #[test]
    fn continuation_lines_and_escaped_spaces() {
        let text = "t1 t2 : a.sv \\\n  b\\ c.svh \\\n  d.sv\n";
        assert_eq!(deps(text), ["a.sv", "b c.svh", "d.sv"]);
    }

    #[test]
    fn rejects_malformed_files() {
        let msg = |text: &str| parse(Path::new("x.d"), text).unwrap_err().to_string();
        assert_eq!(msg(""), "x.d:0: empty dependency file");
        assert_eq!(
            msg("a b c\n"),
            "x.d:1: missing ` : ` between targets and prerequisites"
        );
        assert_eq!(
            msg("a : b\nc : d\n"),
            "x.d:2: expected exactly one `targets : prerequisites` rule"
        );
    }
}
