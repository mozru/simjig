//! Verilator option files (`-f file`, `-F file`) among a DUT's extra arguments, read the
//! way Verilator 5.052 reads them (`V3Options::parseOptsFile`), so the configuration
//! checks see the options inside them too.
//!
//! A file is bytes (an `std::ifstream`, no encoding): a Latin-1 comment is no reason to
//! skip a file, and a path is the exact bytes Verilator opens. Lines end at `\n` only
//! (`std::getline`), so the `\r` of a CRLF file stays in the line, where it is whitespace
//! like any other `isspace` byte (ASCII space, `\t`, `\n`, `\v`, `\f`, `\r`; no other byte
//! is) except after a `\` or inside quotes, where it is part of the word.
//!
//! A word is what is left between whitespace once the comments are gone: `//` after
//! whitespace or at the start of a line, `#` as the first non-blank character of a line,
//! and `/* */`. A file with a `// verilator fargs <words>` (or one-line
//! `/* verilator fargs <words> */`) comment is only the words of those comments. Within a
//! word, `\` escapes the next byte, `"..."` quotes (with `\` escapes inside; the quotes
//! are dropped), and `'"..."'` quotes keeping the double quotes; a `'` not followed by `"`
//! is an ordinary byte (`8'hff`). Checked against the installed 5.052. Paths in an `-F`
//! file are relative to its directory, in an `-f` file to the working directory
//! (`parseFileArg`). `$VAR` in paths is not substituted, and a file that cannot be read
//! is left to Verilator to report.
//!
//! The checks see a word as UTF-8, lossily (an option name is ASCII). A path that is not
//! UTF-8 cannot be tracked, because a Cargo directive is a line of UTF-8, so [`untracked`]
//! names it for the configuration check to refuse.

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
    /// The word, as Verilator reads it (quotes and escapes resolved), as lossy UTF-8.
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
    each_input(args, |path, _| out.push(path));
    out
}

/// The first of [`inputs`] that is not UTF-8, which Cargo cannot track: with the option
/// file and line that name it, the word naming it, and the path.
pub(crate) fn untracked(args: &[String]) -> Option<Found<PathBuf>> {
    let mut first = None;
    each_input(args, |path, at| {
        if first.is_none()
            && path.to_str().is_none()
            && let Some((file, line, word)) = at
        {
            first = Some(Found {
                file: file.to_path_buf(),
                line,
                word: word.to_owned(),
                why: path,
            });
        }
    });
    first
}

/// The walk behind [`inputs`]: each path, with the option file, line and word that name
/// it (`None` in `args`).
fn each_input(args: &[String], mut each: impl FnMut(PathBuf, Option<(&Path, usize, &str)>)) {
    // `-v` or `-y` waiting for its value, the next word of the same list.
    let mut takes_value = false;
    traverse(args, &mut |item| -> Option<()> {
        match item {
            Item::File { path, named_in } => {
                takes_value = false;
                each(path.to_path_buf(), named_in);
            }
            Item::Word {
                word,
                raw,
                base,
                file,
                line,
                ..
            } => {
                let at = file.map(|f| (f, line, word));
                let resolve = |w: &[u8]| {
                    let p = path_of(w);
                    base.map_or_else(|| p.clone(), |d| d.join(&p))
                };
                let flag = word.strip_prefix("--").or_else(|| word.strip_prefix('-'));
                if std::mem::take(&mut takes_value) {
                    each(resolve(raw), at);
                } else if matches!(flag, Some("v" | "y")) {
                    takes_value = true;
                } else if let Some(dir) = raw.strip_prefix(b"-I").filter(|d| !d.is_empty()) {
                    each(resolve(dir), at);
                } else if let Some(dirs) = raw.strip_prefix(b"+incdir+") {
                    for dir in dirs.split(|&b| b == b'+').filter(|d| !d.is_empty()) {
                        each(resolve(dir), at);
                    }
                } else if flag.is_none()
                    && !word.starts_with('+')
                    && SOURCE_SUFFIXES.iter().any(|s| raw.ends_with(s.as_bytes()))
                {
                    each(resolve(raw), at);
                }
            }
        }
        None
    });
}

/// The path of an option file's word, byte for byte where the platform allows it.
#[cfg(unix)]
fn path_of(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_of(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// The suffixes of the positional files Verilator reads (`V3Options::parseOptsList`: C++
/// sources, `.vlt` control files, and the rest as Verilog; Verilog by its usual suffixes).
const SOURCE_SUFFIXES: [&str; 10] = [
    ".v", ".sv", ".vh", ".svh", ".vlt", ".cpp", ".cxx", ".cc", ".c", ".sp",
];

/// What [`traverse`] visits, in Verilator's reading order.
enum Item<'a> {
    /// An option file (`-f`/`-F`), before its words; it may not exist. `named_in` is the
    /// option file, line and word that name it (`None` in `args`).
    File {
        path: &'a Path,
        named_in: Option<(&'a Path, usize, &'a str)>,
    },
    /// Any other word: `word` as lossy UTF-8, `raw` its bytes. `base` is the directory
    /// its relative paths join (an `-F` file's), `None` for the working directory; `file`
    /// is the option file it is in and `line` the 1-based line it starts on there (0 in
    /// `args`); `next` is the word after it in the same list.
    Word {
        word: &'a str,
        raw: &'a [u8],
        next: Option<&'a str>,
        base: Option<&'a Path>,
        file: Option<&'a Path>,
        line: usize,
    },
}

/// A word of a list, as bytes, with the 1-based line it starts on (0 in `args`).
type Words = Vec<(Vec<u8>, usize)>;

/// The one walk over `args` and the option files they name (nested ones too), depth
/// first in order, until `visit` returns `Some`. Verilator 5.052 (`V3Options`, the `-f`
/// and `-F` options): the file an `-f`/`-F` names resolves like any path of the list it
/// is in; the paths inside an `-F` file resolve against its directory, inside an `-f`
/// file against the working directory, even when that `-f` is inside an `-F` file
/// (`parseOptsFile(.., false)` sets the option directory to `.`).
fn traverse<T>(args: &[String], visit: &mut impl FnMut(Item<'_>) -> Option<T>) -> Option<T> {
    let words: Words = args.iter().map(|a| (a.as_bytes().to_vec(), 0)).collect();
    traverse_in(&words, None, None, 0, visit)
}

fn traverse_in<T>(
    words: &Words,
    base: Option<&Path>,
    file: Option<&Path>,
    depth: u32,
    visit: &mut impl FnMut(Item<'_>) -> Option<T>,
) -> Option<T> {
    if depth > MAX_DEPTH {
        return None;
    }
    let text: Vec<String> = words
        .iter()
        .map(|(w, _)| String::from_utf8_lossy(w).into_owned())
        .collect();
    let mut i = 0;
    while let Some(arg) = text.get(i) {
        let line = words[i].1;
        let relative = match arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            Some("f") => false,
            Some("F") => true,
            _ => {
                let found = visit(Item::Word {
                    word: arg,
                    raw: &words[i].0,
                    next: text.get(i + 1).map(String::as_str),
                    base,
                    file,
                    line,
                });
                if found.is_some() {
                    return found;
                }
                i += 1;
                continue;
            }
        };
        i += 1;
        let (named, named_line) = words.get(i)?;
        let named = path_of(named);
        let path = base.map_or_else(|| named.clone(), |dir| dir.join(&named));
        let named_in = file.map(|f| (f, *named_line, text[i].as_str()));
        if let Some(found) = visit(Item::File {
            path: &path,
            named_in,
        }) {
            return Some(found);
        }
        let inner = std::fs::read(&path).map_or_else(|_| Vec::new(), |b| words_of(&b));
        let dir = if relative { path.parent() } else { None };
        if let Some(found) = traverse_in(&inner, dir, Some(&path), depth + 1, visit) {
            return Some(found);
        }
        i += 1;
    }
    None
}

/// C's `isspace` in the C locale, which Verilator's reader uses: ASCII only.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The words of an option file with the line each starts on (see the module docs):
/// `V3Options::parseOptsFile` of 5.052, its comment pass and then its word splitter.
fn words_of(text: &[u8]) -> Words {
    split_words(&without_comments(text))
}

/// The bytes of `text` that are not comments, each with its 1-based line, a space ending
/// each line; or, when the file has `verilator fargs` comments, only their text.
fn without_comments(text: &[u8]) -> Vec<(u8, usize)> {
    let mut kept: Vec<(u8, usize)> = Vec::new();
    let mut fargs = false;
    let mut in_comment = false;
    for (n, line) in text.split(|&b| b == b'\n').enumerate() {
        let n = n + 1;
        let (mut last, mut blank_so_far, mut i) = (b' ', true, 0);
        while let Some(&c) = line.get(i) {
            let next = line.get(i + 1).copied();
            if in_comment {
                if c == b'*' && next == Some(b'/') {
                    in_comment = false;
                    i += 1;
                }
            } else if (c == b'/' && next == Some(b'/') && is_space(last))
                || (c == b'#' && blank_so_far)
            {
                // A comment to the end of the line.
                if c == b'/' {
                    fargs_comment(&line[i..], n, &mut fargs, &mut kept);
                }
                break;
            } else if c == b'/' && next == Some(b'*') {
                fargs_comment(&line[i..], n, &mut fargs, &mut kept);
                in_comment = true;
                blank_so_far = false;
                i += 1;
            } else {
                blank_so_far &= is_space(c);
                if !fargs {
                    kept.push((c, n));
                }
            }
            last = line[i];
            i += 1;
        }
        if !fargs {
            kept.push((b' ', n));
        }
    }
    kept
}

/// A `// verilator fargs <words>` or `/* verilator fargs <words> */` comment (`comment`
/// starts at its `/`): the first one replaces what was kept so far, and from then on only
/// such comments are kept.
fn fargs_comment(comment: &[u8], line: usize, fargs: &mut bool, kept: &mut Vec<(u8, usize)>) {
    let block = comment.get(1) == Some(&b'*');
    let skip_space = |s: &'_ [u8]| -> usize { s.iter().take_while(|&&b| is_space(b)).count() };
    let after = |s: &[u8], word: &[u8]| -> Option<usize> {
        let rest = s.strip_prefix(word)?;
        let n = skip_space(rest);
        (n > 0).then_some(s.len() - rest.len() + n)
    };
    let mut at = 2 + skip_space(&comment[2..]);
    let Some(n) = after(&comment[at..], b"verilator") else {
        return;
    };
    at += n;
    let Some(n) = after(&comment[at..], b"fargs") else {
        return;
    };
    let mut rest = &comment[at + n..];
    if block {
        // A block that does not end on its line is a fatal error, which Verilator reports.
        if let Some(end) = rest.windows(2).position(|w| w == b"*/") {
            rest = &rest[..end];
        }
    }
    if !std::mem::replace(fargs, true) {
        kept.clear();
    }
    kept.extend(rest.iter().chain(b" ").map(|&c| (c, line)));
}

/// Verilator's word splitter: whitespace ends a word outside quotes, `\` escapes a byte,
/// `"..."` quotes with escapes, `'"...'` quotes keeping the `"`.
fn split_words(bytes: &[(u8, usize)]) -> Words {
    #[derive(Clone, Copy)]
    enum St {
        Word,
        Escaped,
        Single,
        Double,
    }
    let (mut st, mut before_escape) = (St::Word, St::Word);
    let mut out = Vec::new();
    let mut word = Vec::new();
    let mut start = 0;
    let mut it = bytes.iter().copied();
    while let Some((c, line)) = it.next() {
        if word.is_empty() && matches!(st, St::Word) {
            start = line;
        }
        match st {
            St::Word if is_space(c) => {
                if !word.is_empty() {
                    out.push((std::mem::take(&mut word), start));
                }
            }
            St::Word if c == b'\\' => (before_escape, st) = (st, St::Escaped),
            St::Word if c == b'\'' => {
                // `'"` opens a quoted string; otherwise the quote is kept (`8'hff`), and
                // so is the byte after it, whatever it is.
                let after = it.next().map_or(b'\'', |(a, _)| a);
                if after == b'"' {
                    st = St::Single;
                } else {
                    word.push(b'\'');
                }
                word.push(after);
            }
            St::Word if c == b'"' => st = St::Double,
            St::Single if c == b'\'' => st = St::Word,
            St::Double if c == b'"' => st = St::Word,
            St::Double if c == b'\\' => (before_escape, st) = (st, St::Escaped),
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

    /// A path in a Latin-1 list is the bytes Verilator opens, and, since Cargo cannot
    /// track it, [`untracked`] names it with its line; a non-UTF-8 comment or option
    /// value is no path.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_path_is_exact_and_untracked() {
        use std::os::unix::ffi::OsStrExt as _;
        let t = tempfile::tempdir().unwrap();
        let list = t.path().join("list.F");
        std::fs::write(
            &list,
            b"// J\xf6rg\n+define+WHO=J\xf6rg\nok.sv\nj\xf6rg.sv\n",
        )
        .unwrap();
        let args = vec!["-F".to_owned(), list.display().to_string()];
        let latin1 = t.path().join(std::ffi::OsStr::from_bytes(b"j\xf6rg.sv"));
        assert_eq!(
            inputs(&args),
            [list.clone(), t.path().join("ok.sv"), latin1.clone()]
        );
        assert_eq!(
            untracked(&args),
            Some(Found {
                file: list.clone(),
                line: 4,
                word: "j\u{fffd}rg.sv".to_owned(),
                why: latin1
            })
        );
        std::fs::write(&list, b"// J\xf6rg\nok.sv\n").unwrap();
        assert_eq!(untracked(&args), None);
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

    fn words(text: impl AsRef<[u8]>) -> Vec<String> {
        words_of(text.as_ref())
            .into_iter()
            .map(|(w, _)| String::from_utf8_lossy(&w).into_owned())
            .collect()
    }

    /// Bytes as the installed 5.052 reads them, each case checked by running it on an
    /// option file and reading the words back from `Cannot find file containing module`:
    /// a CRLF line keeps its `\r`, which is whitespace except after a `\` or inside
    /// quotes; whitespace is C's ASCII `isspace` (`\v` and `\f` split, `\xa0`, `\x85`
    /// and `\x1c` do not); and a byte that is not UTF-8 neither hides a word nor ends one.
    #[test]
    fn bytes_are_verilators() {
        assert_eq!(
            words(b"r1\r\nr2\\\r\nr3 \"q\r\nx\" r4\r\n// -c\r\n\r#h\r\n"),
            ["r1", "r2\r", "r3", "q\r x", "r4"]
        );
        assert_eq!(
            words(b"a\xa0b c\x85d e\x0bf g\x0ch i\x1cj\n"),
            ["a\u{fffd}b", "c\u{fffd}d", "e", "f", "g", "h", "i\x1cj"]
        );
        assert_eq!(
            words_of(b"// J\xf6rg\n--top-module x\xf6\n"),
            [(b"--top-module".to_vec(), 2), (b"x\xf6".to_vec(), 2)]
        );
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
            words_of(b"a.sv\n\n  --top-module x // c\n\"y\nz\"\n"),
            [
                (b"a.sv".to_vec(), 1),
                (b"--top-module".to_vec(), 3),
                (b"x".to_vec(), 3),
                (b"y z".to_vec(), 4)
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
