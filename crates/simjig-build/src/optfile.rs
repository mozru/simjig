//! Verilator option files (`-f file`, `-F file`) among a DUT's extra arguments, read the
//! way Verilator 5.052 reads them (`V3Options::parseOptsFile`), so the configuration
//! checks see the options inside them too.
//!
//! A word is what is left between whitespace once the comments are gone: `//` after
//! whitespace or at the start of a line, `#` as the first non-blank character of a line,
//! and `/* */`. A file with a `// verilator fargs <words>` (or one-line
//! `/* verilator fargs <words> */`) comment is only the words of those comments. Within a
//! word, `\` escapes the next character, `"..."` quotes (with `\` escapes inside; the
//! quotes are dropped), and `'"..."'` quotes keeping the double quotes; a `'` not followed
//! by `"` is an ordinary character (`8'hff`). Checked against the
//! installed 5.052. Paths in an `-F` file are relative to its directory, in an `-f` file
//! to the working directory (`parseFileArg`). `$VAR` in paths is not substituted, and a
//! file that cannot be read is left to Verilator to report.

use std::path::{Path, PathBuf};

/// How deep option files may nest before the search stops (a file including itself).
const MAX_DEPTH: u32 = 16;

/// A word of an option file that a check refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Found<T> {
    /// The option file the word is in.
    pub(crate) file: PathBuf,
    /// The 1-based line of the file the word starts on.
    pub(crate) line: usize,
    /// The word, as Verilator reads it (quotes and escapes resolved).
    pub(crate) word: String,
    /// What the check said about it.
    pub(crate) why: T,
}

/// The first word of the option files `args` name (nested ones too) for which `check`,
/// given the word and the next word of the same file, returns `Some`. Words of `args`
/// themselves are not checked.
pub(crate) fn find<T>(
    args: &[String],
    mut check: impl FnMut(&str, Option<&str>) -> Option<T>,
) -> Option<Found<T>> {
    traverse(args, &mut |item| match item {
        Item::Word {
            word,
            next,
            file: Some(file),
            line,
            ..
        } => check(word, next).map(|why| Found {
            file: file.to_path_buf(),
            line,
            word: word.to_owned(),
            why,
        }),
        _ => None,
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
    // `-v` or `-y` waiting for its value, the next word of the same list.
    let mut takes_value = false;
    traverse(args, &mut |item| -> Option<()> {
        match item {
            Item::File(path) => {
                takes_value = false;
                out.push(path.to_path_buf());
            }
            Item::Word { word, base, .. } => {
                let resolve = |w: &str| base.map_or_else(|| PathBuf::from(w), |d| d.join(w));
                let flag = word.strip_prefix("--").or_else(|| word.strip_prefix('-'));
                if std::mem::take(&mut takes_value) {
                    out.push(resolve(word));
                } else if matches!(flag, Some("v" | "y")) {
                    takes_value = true;
                } else if let Some(dir) = word.strip_prefix("-I").filter(|d| !d.is_empty()) {
                    out.push(resolve(dir));
                } else if let Some(dirs) = word.strip_prefix("+incdir+") {
                    out.extend(dirs.split('+').filter(|d| !d.is_empty()).map(resolve));
                } else if flag.is_none()
                    && !word.starts_with('+')
                    && SOURCE_SUFFIXES.iter().any(|s| word.ends_with(s))
                {
                    out.push(resolve(word));
                }
            }
        }
        None
    });
    out
}

/// The suffixes of the positional files Verilator reads (`V3Options::parseOptsList`: C++
/// sources, `.vlt` control files, and the rest as Verilog; Verilog by its usual suffixes).
const SOURCE_SUFFIXES: [&str; 10] = [
    ".v", ".sv", ".vh", ".svh", ".vlt", ".cpp", ".cxx", ".cc", ".c", ".sp",
];

/// What [`traverse`] visits, in Verilator's reading order.
enum Item<'a> {
    /// An option file (`-f`/`-F`), before its words; it may not exist.
    File(&'a Path),
    /// Any other word. `base` is the directory its relative paths join (an `-F` file's),
    /// `None` for the working directory; `file` is the option file it is in and `line`
    /// the 1-based line it starts on there (0 in `args`); `next` is the word after it in
    /// the same list.
    Word {
        word: &'a str,
        next: Option<&'a str>,
        base: Option<&'a Path>,
        file: Option<&'a Path>,
        line: usize,
    },
}

/// The one walk over `args` and the option files they name (nested ones too), depth
/// first in order, until `visit` returns `Some`. Verilator 5.052 (`V3Options`, the `-f`
/// and `-F` options): the file an `-f`/`-F` names resolves like any path of the list it
/// is in; the paths inside an `-F` file resolve against its directory, inside an `-f`
/// file against the working directory, even when that `-f` is inside an `-F` file
/// (`parseOptsFile(.., false)` sets the option directory to `.`).
fn traverse<T>(args: &[String], visit: &mut impl FnMut(Item<'_>) -> Option<T>) -> Option<T> {
    let words: Vec<(String, usize)> = args.iter().map(|a| (a.clone(), 0)).collect();
    traverse_in(&words, None, None, 0, visit)
}

fn traverse_in<T>(
    words: &[(String, usize)],
    base: Option<&Path>,
    file: Option<&Path>,
    depth: u32,
    visit: &mut impl FnMut(Item<'_>) -> Option<T>,
) -> Option<T> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut it = words.iter().enumerate();
    while let Some((i, (arg, line))) = it.next() {
        let relative = match arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            Some("f") => false,
            Some("F") => true,
            _ => {
                let found = visit(Item::Word {
                    word: arg,
                    next: words.get(i + 1).map(|(w, _)| w.as_str()),
                    base,
                    file,
                    line: *line,
                });
                if found.is_some() {
                    return found;
                }
                continue;
            }
        };
        let named = Path::new(&it.next()?.1.0);
        let path = base.map_or_else(|| named.to_path_buf(), |dir| dir.join(named));
        if let Some(found) = visit(Item::File(&path)) {
            return Some(found);
        }
        let inner = std::fs::read_to_string(&path).map_or_else(|_| Vec::new(), |t| words_of(&t));
        let dir = if relative { path.parent() } else { None };
        if let Some(found) = traverse_in(&inner, dir, Some(&path), depth + 1, visit) {
            return Some(found);
        }
    }
    None
}

/// The words of an option file with the line each starts on (see the module docs):
/// `V3Options::parseOptsFile` of 5.052, its comment pass and then its word splitter.
fn words_of(text: &str) -> Vec<(String, usize)> {
    split_words(&without_comments(text))
}

/// The characters of `text` that are not comments, each with its 1-based line, a space
/// ending each line; or, when the file has `verilator fargs` comments, only their text.
fn without_comments(text: &str) -> Vec<(char, usize)> {
    let mut kept: Vec<(char, usize)> = Vec::new();
    let mut fargs = false;
    let mut in_comment = false;
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
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
                if c == '/' {
                    fargs_comment(&chars[i..], n, &mut fargs, &mut kept);
                }
                break;
            } else if c == '/' && next == Some('*') {
                fargs_comment(&chars[i..], n, &mut fargs, &mut kept);
                in_comment = true;
                blank_so_far = false;
                i += 1;
            } else {
                blank_so_far &= c.is_whitespace();
                if !fargs {
                    kept.push((c, n));
                }
            }
            last = chars[i];
            i += 1;
        }
        if !fargs {
            kept.push((' ', n));
        }
    }
    kept
}

/// A `// verilator fargs <words>` or `/* verilator fargs <words> */` comment (`comment`
/// starts at its `/`): the first one replaces what was kept so far, and from then on only
/// such comments are kept.
fn fargs_comment(comment: &[char], line: usize, fargs: &mut bool, kept: &mut Vec<(char, usize)>) {
    let block = comment.get(1) == Some(&'*');
    let text: String = comment[2..].iter().collect();
    let Some(rest) = text
        .trim_start()
        .strip_prefix("verilator")
        .filter(|r| r.starts_with(char::is_whitespace))
        .and_then(|r| r.trim_start().strip_prefix("fargs"))
        .filter(|r| r.starts_with(char::is_whitespace))
    else {
        return;
    };
    let mut rest = rest.trim_start();
    if block {
        // A block that does not end on its line is a fatal error, which Verilator reports.
        rest = rest.split_once("*/").map_or(rest, |(words, _)| words);
    }
    if !std::mem::replace(fargs, true) {
        kept.clear();
    }
    kept.extend(rest.chars().chain([' ']).map(|c| (c, line)));
}

/// Verilator's word splitter: whitespace ends a word outside quotes, `\` escapes a
/// character, `"..."` quotes with escapes, `'"...'` quotes keeping the `"`.
fn split_words(chars: &[(char, usize)]) -> Vec<(String, usize)> {
    #[derive(Clone, Copy)]
    enum St {
        Word,
        Escaped,
        Single,
        Double,
    }
    let (mut st, mut before_escape) = (St::Word, St::Word);
    let mut out = Vec::new();
    let mut word = String::new();
    let mut start = 0;
    let mut it = chars.iter().copied().peekable();
    while let Some((c, line)) = it.next() {
        if word.is_empty() && matches!(st, St::Word) {
            start = line;
        }
        match st {
            St::Word if c.is_whitespace() => {
                if !word.is_empty() {
                    out.push((std::mem::take(&mut word), start));
                }
            }
            St::Word if c == '\\' => (before_escape, st) = (st, St::Escaped),
            St::Word if c == '\'' => {
                // `'"` opens a quoted string; otherwise the quote is kept (`8'hff`), and
                // so is the character after it, whatever it is.
                let after = it.next().map_or('\'', |(a, _)| a);
                if after == '"' {
                    st = St::Single;
                } else {
                    word.push('\'');
                }
                word.push(after);
            }
            St::Word if c == '"' => st = St::Double,
            St::Single if c == '\'' => st = St::Word,
            St::Double if c == '"' => st = St::Word,
            St::Double if c == '\\' => (before_escape, st) = (st, St::Escaped),
            St::Escaped => {
                word.push(c);
                st = before_escape;
            }
            St::Word | St::Single | St::Double => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push((word, start));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_g(word: &str, _: Option<&str>) -> Option<()> {
        word.starts_with("-G").then_some(())
    }

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

    /// An `-f` file named inside an `-F` file is found next to the `-F` file, but its own
    /// paths are relative to the working directory (`parseOptsFile(.., false)` sets the
    /// option directory to `.`; checked against Verilator 5.052), not to the `-F` file's.
    #[test]
    fn an_f_file_inside_an_upper_f_file_resolves_against_the_working_directory() {
        let t = tempfile::tempdir().unwrap();
        let sub = t.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("outer.F"), "-f inner.f\n").unwrap();
        std::fs::write(sub.join("inner.f"), "top.sv\n-GX=1\n").unwrap();
        let outer = vec!["-F".to_owned(), sub.join("outer.F").display().to_string()];
        assert_eq!(
            inputs(&outer),
            [
                sub.join("outer.F"),
                sub.join("inner.f"),
                PathBuf::from("top.sv")
            ]
        );
        assert_eq!(
            find(&outer, is_g),
            Some(Found {
                file: sub.join("inner.f"),
                line: 2,
                word: "-GX=1".to_owned(),
                why: ()
            })
        );
    }

    fn words(text: &str) -> Vec<String> {
        words_of(text).into_iter().map(|(w, _)| w).collect()
    }

    #[test]
    fn comments_are_not_words() {
        let text = "-Wall // -GA=1\n# -GB=1\n  # -GC=1\n-x /* -GD=1\n -GE=1 */ -y\n\
                    a//b \"-GF=1\" #z\n";
        assert_eq!(words(text), ["-Wall", "-x", "-y", "a//b", "-GF=1", "#z"]);
    }

    /// Quotes and escapes as the installed Verilator 5.052 reads them, each case checked by
    /// running it on an option file and reading the word back from `Invalid option: ..`.
    #[test]
    fn quotes_and_escapes_are_verilators() {
        for (text, want) in [
            (r"--bog\ us", &["--bog us"][..]),
            (r#""--bog us""#, &["--bog us"]),
            (r#"--a"b c"d"#, &["--ab cd"]),
            (r#""a\"b""#, &[r#"a"b"#]),
            (r#"'"x y"'"#, &[r#""x y""#]),
            // A `'` keeps the character after it, the line's closing space here (5.052
            // reports `Cannot find file containing module: 'us' '`).
            ("'--bog us'", &["'--bog", "us' "]),
            ("-GX='h5 --bogq", &["-GX='h5", "--bogq"]),
        ] {
            assert_eq!(words(text), want, "{text}");
        }
    }

    /// A `verilator fargs` comment (5.052 reads them; checked) makes the file only those
    /// comments' words.
    #[test]
    fn fargs_comments_replace_the_file() {
        assert_eq!(
            words("-a\n// verilator fargs --fargsword\n--bodyword\n"),
            ["--fargsword"]
        );
        assert_eq!(
            words("/* verilator fargs --blockfargs */ --body2\n// verilator fargs x y\n"),
            ["--blockfargs", "x", "y"]
        );
        assert_eq!(words("// verilator fargsx --a\n-b\n"), ["-b"]);
    }

    #[test]
    fn words_know_their_line() {
        assert_eq!(
            words_of("a.sv\n\n  --top-module x // c\n\"y\nz\"\n"),
            [
                ("a.sv".to_owned(), 1),
                ("--top-module".to_owned(), 3),
                ("x".to_owned(), 3),
                ("y z".to_owned(), 4)
            ]
        );
    }

    #[test]
    fn nested_files_are_searched_relative_to_their_rule() {
        let t = tempfile::tempdir().unwrap();
        let sub = t.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        // `-F` resolves `inner.f` next to itself.
        std::fs::write(sub.join("outer.F"), "-Wall\n-f inner.f\n").unwrap();
        std::fs::write(sub.join("inner.f"), "// -GX=0\n-GX=1\n").unwrap();
        let outer = sub.join("outer.F").display().to_string();
        assert_eq!(
            find(&["-F".to_owned(), outer.clone()], is_g),
            Some(Found {
                file: sub.join("inner.f"),
                line: 2,
                word: "-GX=1".to_owned(),
                why: ()
            })
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
