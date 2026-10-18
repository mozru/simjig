//! Minimal JSON: string writing for `--json` output, and reading one string field of
//! Cargo's JSON messages.
//!
//! The output is a fixed shape of strings, booleans and arrays, so escaping strings per
//! RFC 8259 §7 is the only non-trivial part; that does not justify a serde dependency.
//! Reading needs only top-level string fields of Cargo's one-line messages
//! (`"out_dir":"..."`), which Cargo prints without whitespace between tokens.

/// Appends `text` to `out` as a quoted JSON string.
pub(crate) fn push_str(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // RFC 8259 §7: every other control character must be escaped as \u00XX.
            c if c < '\u{20}' => {
                let hex = |digit: u32| char::from_digit(digit, 16).unwrap_or('0');
                out.push_str("\\u00");
                out.push(hex(u32::from(c) >> 4));
                out.push(hex(u32::from(c) & 0xf));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The value of the first string field `key` in the one-line JSON object `line`, unescaped
/// (RFC 8259 §7). `None` when there is no such field, or its value is not a string.
///
/// A field of a nested object counts too; Cargo's `build-script-executed` messages have
/// no nested `reason` or `out_dir`.
pub(crate) fn find_str(line: &str, key: &str) -> Option<String> {
    let start = line.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let mut out = String::new();
    let mut chars = line[start..].chars();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_unescapes_a_string_field() {
        let line = r#"{"reason":"build-script-executed","out_dir":"/a \"b\"\\c\u0041","n":1}"#;
        assert_eq!(
            find_str(line, "reason").as_deref(),
            Some("build-script-executed")
        );
        assert_eq!(find_str(line, "out_dir").as_deref(), Some("/a \"b\"\\cA"));
        assert_eq!(find_str(line, "n"), None, "not a string");
        assert_eq!(find_str(line, "missing"), None);
        assert_eq!(find_str(r#"{"out_dir":"/unterminated"#, "out_dir"), None);
    }

    #[test]
    fn escapes_per_rfc_8259() {
        let check = |text: &str, expect: &str| {
            let mut out = String::new();
            push_str(&mut out, text);
            assert_eq!(out, expect, "{text:?}");
        };
        check("", r#""""#);
        check("plain /path ok", r#""plain /path ok""#);
        check(r#"say "hi""#, r#""say \"hi\"""#);
        check(r"C:\dir", r#""C:\\dir""#);
        check("a\nb\r\tc", r#""a\nb\r\tc""#);
        check("\u{0}\u{1b}\u{1f}", r#""\u0000\u001b\u001f""#);
        check("µs \u{7f}", "\"µs \u{7f}\"");
    }
}
