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
    find_in(args, None, refused, 0)
}

/// `base`: the directory relative paths are joined to (an `-F` file's), else the
/// working directory.
fn find_in(
    args: &[String],
    base: Option<&Path>,
    refused: fn(&str) -> bool,
    depth: u32,
) -> Option<(PathBuf, String)> {
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
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let words = words(&text);
        if let Some(w) = words.iter().find(|w| refused(w)) {
            return Some((path, w.clone()));
        }
        let dir = if relative { path.parent() } else { base };
        if let Some(found) = find_in(&words, dir, refused, depth + 1) {
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
    }
}
