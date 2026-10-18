//! Verilator option files (`-f file`, `-F file`) among a DUT's extra arguments, read the
//! way Verilator 5.052 reads them (`V3Options::parseOptsFile`), so the configuration
//! checks see the options inside them too.
//!
//! A word is what is left between whitespace once the comments are gone: `//` after
//! whitespace or at the start of a line, `#` as the first non-blank character of a line,
//! and `/* */`. Quotes and backslash escapes are not interpreted (a quote at either end
//! of a word is dropped), which only matters to an option inside a quoted value. Paths in
//! an `-F` file are relative to its directory, in an `-f` file to the working directory
//! (`parseFileArg`). `$VAR` in paths is not substituted, and a file that cannot be read
//! is left to Verilator to report.

use std::path::{Path, PathBuf};

/// How deep option files may nest before the search stops (a file including itself).
const MAX_DEPTH: u32 = 16;

/// The first word of the option files `args` name (nested ones too) that `refused`
/// accepts, with the file it is in.
pub(crate) fn find(args: &[String], refused: fn(&str) -> bool) -> Option<(PathBuf, String)> {
    walk(args, None, 0, &mut |path, words| {
        let w = words.iter().find(|w| refused(w))?;
        Some((path.to_path_buf(), w.clone()))
    })
}

/// Every input `args` name, for the reruns of a failed build, readable or not (a missing
/// one may be created later): each option file (nested ones too), and, in `args` and in
/// those files, what Verilator reads by path (`V3Options::parseOpts`):
/// - a positional word with a source suffix (`.v`, `.sv`, `.vh`, `.svh`, `.vlt`, and the
///   C++ ones Verilator compiles);
/// - the value of `-v` (a library file) and `-y` (a library directory);
/// - the directories of `-I<dir>` and `+incdir+<dir>[+<dir>..]`.
///
/// Relative paths resolve as Verilator's `parseFileArg` does them: in an `-F` file
/// against its directory, elsewhere against the working directory. A positional word
/// is recognised by its suffix because only Verilator's option table knows which options
/// take a value (`--timescale 1ns/1ps`); a word it misses is not tracked.
pub(crate) fn inputs(args: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(args, None, 0, &mut out);
    out
}

/// The suffixes of the positional files Verilator reads (`V3Options::parseOptsList`: C++
/// sources, `.vlt` control files, and the rest as Verilog; Verilog by its usual suffixes).
const SOURCE_SUFFIXES: [&str; 10] = [
    ".v", ".sv", ".vh", ".svh", ".vlt", ".cpp", ".cxx", ".cc", ".c", ".sp",
];

/// [`inputs`] of `args` with relative paths joined to `base` (an `-F` file's directory).
fn collect(args: &[String], base: Option<&Path>, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let resolve = |word: &str| base.map_or_else(|| PathBuf::from(word), |dir| dir.join(word));
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'));
        match flag {
            Some(f @ ("f" | "F")) => {
                let Some(named) = it.next() else { return };
                let path = resolve(named);
                out.push(path.clone());
                let words =
                    std::fs::read_to_string(&path).map_or_else(|_| Vec::new(), |t| words(&t));
                let dir = if f == "F" { path.parent() } else { base };
                collect(&words, dir, depth + 1, out);
            }
            Some("v" | "y") => out.extend(it.next().map(|v| resolve(v))),
            _ => {
                if let Some(dir) = arg.strip_prefix("-I").filter(|d| !d.is_empty()) {
                    out.push(resolve(dir));
                } else if let Some(dirs) = arg.strip_prefix("+incdir+") {
                    out.extend(dirs.split('+').filter(|d| !d.is_empty()).map(resolve));
                } else if flag.is_none()
                    && !arg.starts_with('+')
                    && SOURCE_SUFFIXES.iter().any(|s| arg.ends_with(s))
                {
                    out.push(resolve(arg));
                }
            }
        }
    }
}

/// Calls `visit` on each option file `args` name, depth first in order, with its words
/// (none for a file that cannot be read), until `visit` returns `Some`.
///
/// `base`: the directory relative paths are joined to (an `-F` file's), else the
/// working directory.
fn walk<T>(
    args: &[String],
    base: Option<&Path>,
    depth: u32,
    visit: &mut impl FnMut(&Path, &[String]) -> Option<T>,
) -> Option<T> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let relative = match arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            Some("f") => false,
            Some("F") => true,
            _ => continue,
        };
        let named = Path::new(it.next()?);
        let path = base.map_or_else(|| named.to_path_buf(), |dir| dir.join(named));
        let words = std::fs::read_to_string(&path).map_or_else(|_| Vec::new(), |t| words(&t));
        if let Some(found) = visit(&path, &words) {
            return Some(found);
        }
        let dir = if relative { path.parent() } else { base };
        if let Some(found) = walk(&words, dir, depth + 1, visit) {
            return Some(found);
        }
    }
    None
}

/// The words of an option file (see the module docs).
fn words(text: &str) -> Vec<String> {
    let mut kept = String::new();
    let mut in_comment = false;
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        let (mut last, mut blank_so_far, mut i) = (' ', true, 0);
        while let Some(&c) = chars.get(i) {
            let next = chars.get(i + 1).copied();
            if in_comment {
                if c == '*' && next == Some('/') {
                    in_comment = false;
                    i += 1;
                }
            } else if (c == '/' && next == Some('/') && last.is_whitespace())
                || (c == '#' && blank_so_far)
            {
                // A comment to the end of the line.
                break;
            } else if c == '/' && next == Some('*') {
                in_comment = true;
                blank_so_far = false;
                i += 1;
            } else {
                blank_so_far &= c.is_whitespace();
                kept.push(c);
            }
            last = chars[i];
            i += 1;
        }
        kept.push(' ');
    }
    kept.split_whitespace()
        .map(|w| w.trim_matches('"').to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a failed build reruns on: sources, library files and directories and include
    /// directories, from the arguments and from option files, resolved like Verilator;
    /// option values without a source suffix are not paths.
    #[test]
    fn inputs_are_the_paths_verilator_reads() {
        let t = tempfile::tempdir().unwrap();
        let sub = t.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("list.F"),
            "a.sv -y lib -v cells.v +incdir+inc1+inc2 -Iinc3 --top-module top\n",
        )
        .unwrap();
        std::fs::write(t.path().join("list.f"), "b.sv // c.sv\n").unwrap();
        let args: Vec<String> = [
            "--timescale",
            "1ns/1ps",
            "-F",
            &sub.join("list.F").display().to_string(),
            "-f",
            &t.path().join("list.f").display().to_string(),
            "d.svh",
            "-y",
            "ylib",
            "-Wno-fatal",
        ]
        .map(str::to_owned)
        .to_vec();
        let want: Vec<PathBuf> = vec![
            sub.join("list.F"),
            sub.join("a.sv"),
            sub.join("lib"),
            sub.join("cells.v"),
            sub.join("inc1"),
            sub.join("inc2"),
            sub.join("inc3"),
            t.path().join("list.f"),
            // `-f`: relative to the working directory.
            PathBuf::from("b.sv"),
            PathBuf::from("d.svh"),
            PathBuf::from("ylib"),
        ];
        assert_eq!(inputs(&args), want);
    }

    #[test]
    fn comments_are_not_words() {
        let text = "-Wall // -GA=1\n# -GB=1\n  # -GC=1\n-x /* -GD=1\n -GE=1 */ -y\n\
                    a//b \"-GF=1\" #z\n";
        assert_eq!(words(text), ["-Wall", "-x", "-y", "a//b", "-GF=1", "#z"]);
    }

    #[test]
    fn nested_files_are_searched_relative_to_their_rule() {
        let t = tempfile::tempdir().unwrap();
        let sub = t.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        // `-F` resolves `inner.f` next to itself.
        std::fs::write(sub.join("outer.F"), "-Wall\n-f inner.f\n").unwrap();
        std::fs::write(sub.join("inner.f"), "// -GX=0\n-GX=1\n").unwrap();
        let is_g = |w: &str| w.starts_with("-G");
        let outer = sub.join("outer.F").display().to_string();
        assert_eq!(
            find(&["-F".to_owned(), outer.clone()], is_g),
            Some((sub.join("inner.f"), "-GX=1".to_owned()))
        );
        assert_eq!(
            inputs(&[
                "-F".to_owned(),
                outer.clone(),
                "-f".to_owned(),
                "missing.f".to_owned()
            ]),
            [
                sub.join("outer.F"),
                sub.join("inner.f"),
                PathBuf::from("missing.f")
            ]
        );
        // `-f` resolves it against the working directory, where there is none.
        std::fs::rename(sub.join("outer.F"), sub.join("outer.f")).unwrap();
        let outer = sub.join("outer.f").display().to_string();
        assert_eq!(find(&["-f".to_owned(), outer], is_g), None);
        // A file that includes itself ends the search.
        let me = t.path().join("me.F");
        std::fs::write(&me, "-F me.F\n").unwrap();
        assert_eq!(
            find(&["-F".to_owned(), me.display().to_string()], is_g),
            None
        );
        assert_eq!(
            inputs(&["-F".to_owned(), me.display().to_string()]).len(),
            usize::try_from(MAX_DEPTH + 1).unwrap(),
            "a self-including file is listed once per level, up to the depth limit"
        );
    }
}
