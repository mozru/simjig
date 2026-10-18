//! Minimal JSON string writing for `--json` output.
//!
//! The output is a fixed shape of strings, booleans and arrays, so escaping strings per
//! RFC 8259 §7 is the only non-trivial part; that does not justify a serde dependency.

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

#[cfg(test)]
mod tests {
    use super::*;

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
