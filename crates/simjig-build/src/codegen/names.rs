//! Rust identifiers for SystemVerilog names.
//!
//! Every generated Rust name comes from this module, called only by the lowering, so the
//! rules live in one place:
//! - fields, methods and modules are `snake_case`, types are `CamelCase`, constants are
//!   `SCREAMING_SNAKE_CASE` (Rust API Guidelines C-CASE);
//! - characters that SystemVerilog escaped identifiers allow but Rust does not become `_`,
//!   and a name starting with a digit gets a prefix;
//! - Rust keywords become raw identifiers (`r#type`), except the four that cannot be raw
//!   (`self`, `Self`, `super`, `crate`), which get a trailing `_`;
//! - a [`Namespace`] makes names unique within one scope by appending `_2`, `_3`, ... in
//!   claim order, which is the IR's declaration order, so output is deterministic.

use std::collections::BTreeSet;

/// Rust keywords of edition 2024, strict and reserved.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Keywords that are not valid raw identifiers.
const NOT_RAW: &[&str] = &["self", "Self", "super", "crate"];

/// Splits `name` into lower-case words at `_`, at characters Rust identifiers cannot
/// hold, and at case changes (`inBeat` and `IN_Beat` both give `in`, `beat`; `ABCDef` gives
/// `abc`, `def`).
fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        let boundary = c.is_ascii_uppercase()
            && prev.is_some_and(|p| {
                p.is_ascii_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase()))
            });
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Makes `ident` a legal Rust identifier: a raw identifier or a `_` suffix for keywords.
fn unkeyword(ident: String) -> String {
    if NOT_RAW.contains(&ident.as_str()) {
        format!("{ident}_")
    } else if KEYWORDS.contains(&ident.as_str()) {
        format!("r#{ident}")
    } else {
        ident
    }
}

/// `snake_case` for fields, methods and modules.
pub(crate) fn snake(name: &str) -> String {
    let joined = words(name).join("_");
    let ident = match joined.chars().next() {
        None => "unnamed".to_owned(),
        Some(c) if c.is_ascii_digit() => format!("_{joined}"),
        Some(_) => joined,
    };
    unkeyword(ident)
}

/// `SCREAMING_SNAKE_CASE` for constants. No keyword is upper case except `Self`.
pub(crate) fn screaming(name: &str) -> String {
    let joined = words(name).join("_").to_ascii_uppercase();
    match joined.chars().next() {
        None => "UNNAMED".to_owned(),
        Some(c) if c.is_ascii_digit() => format!("_{joined}"),
        Some(_) => joined,
    }
}

/// `CamelCase` for types.
pub(crate) fn camel(name: &str) -> String {
    let mut ident: String = words(name)
        .iter()
        .map(|w| {
            let mut cs = w.chars();
            cs.next()
                .map(|c| c.to_ascii_uppercase().to_string() + cs.as_str())
                .unwrap_or_default()
        })
        .collect();
    match ident.chars().next() {
        None => "Unnamed".clone_into(&mut ident),
        Some(c) if c.is_ascii_digit() => ident.insert(0, 'T'),
        Some(_) => {}
    }
    unkeyword(ident)
}

/// The names already used in one Rust scope.
#[derive(Debug, Default)]
pub(crate) struct Namespace {
    used: BTreeSet<String>,
}

impl Namespace {
    /// A namespace in which `reserved` are already taken.
    pub(crate) fn with_reserved(reserved: &[&str]) -> Self {
        Self {
            used: reserved.iter().map(|&s| s.to_owned()).collect(),
        }
    }

    /// Whether `name` is taken.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.used.contains(name)
    }

    /// Claims `ident`, or `ident_2`, `ident_3`, ... if it is taken (`Ident2` for a
    /// `CamelCase` name, so the type stays camel case).
    pub(crate) fn claim(&mut self, ident: &str) -> String {
        let camel = ident.starts_with(|c: char| c.is_ascii_uppercase())
            && !ident.contains('_')
            && ident.chars().any(|c| c.is_ascii_lowercase());
        // A suffixed keyword is no longer a keyword, so the raw prefix goes.
        let bare = ident.strip_prefix("r#").unwrap_or(ident);
        let mut candidate = ident.to_owned();
        let mut n = 2;
        while self.used.contains(&candidate) {
            candidate = if camel {
                format!("{bare}{n}")
            } else {
                format!("{}_{n}", bare.trim_end_matches('_'))
            };
            n += 1;
        }
        self.used.insert(candidate.clone());
        candidate
    }
}

/// The candidate closest to `name` by edit distance, if one is close enough to be a likely
/// typo (at most one edit per four characters, and at least one): for "did you mean"
/// hints in configuration errors. Ties go to the first candidate.
fn closest<'c>(name: &str, candidates: impl IntoIterator<Item = &'c str>) -> Option<&'c str> {
    let limit = 1 + name.chars().count() / 4;
    candidates
        .into_iter()
        .map(|c| (edit_distance(name, c), c))
        .filter(|&(d, c)| d <= limit && c != name)
        .min_by_key(|&(d, _)| d)
        .map(|(_, c)| c)
}

/// `"; did you mean `x`?"` for the [`closest`] candidate, or nothing: the one spelling of
/// the hint every configuration error appends.
pub(crate) fn did_you_mean<'c>(
    name: &str,
    candidates: impl IntoIterator<Item = &'c str>,
) -> String {
    closest(name, candidates)
        .map(|c| format!("; did you mean `{c}`?"))
        .unwrap_or_default()
}

/// Levenshtein distance over chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let next = (row[j] + 1)
                .min(row[j + 1] + 1)
                .min(diag + usize::from(ca != cb));
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases() {
        for (sv, s, c, k) in [
            ("in_beat", "in_beat", "InBeat", "IN_BEAT"),
            ("inBeat", "in_beat", "InBeat", "IN_BEAT"),
            ("ABCDef", "abc_def", "AbcDef", "ABC_DEF"),
            ("DATA_W", "data_w", "DataW", "DATA_W"),
            ("esc$name", "esc_name", "EscName", "ESC_NAME"),
            ("b8", "b8", "B8", "B8"),
            ("7up", "_7up", "T7up", "_7UP"),
            ("type", "r#type", "Type", "TYPE"),
            ("self", "self_", "Self_", "SELF"),
            ("a__b", "a_b", "AB", "A_B"),
            ("$", "unnamed", "Unnamed", "UNNAMED"),
        ] {
            assert_eq!(
                (snake(sv), camel(sv), screaming(sv)),
                (s.into(), c.into(), k.into()),
                "{sv}"
            );
        }
    }

    #[test]
    fn namespace_suffixes_in_claim_order() {
        let mut ns = Namespace::with_reserved(&["Dut"]);
        assert_eq!(ns.claim("data"), "data");
        assert_eq!(ns.claim("data"), "data_2");
        assert_eq!(ns.claim("data"), "data_3");
        assert_eq!(ns.claim("Dut"), "Dut2");
        assert_eq!(ns.claim("r#type"), "r#type");
        assert_eq!(ns.claim("r#type"), "type_2");
        assert_eq!(ns.claim("self_"), "self_");
        assert_eq!(ns.claim("self_"), "self_2");
        assert!(ns.contains("data_2"));
    }

    #[test]
    fn closest_suggests_likely_typos_only() {
        let vars = ["result", "state", "clk"];
        assert_eq!(closest("reslt", vars), Some("result"));
        assert_eq!(closest("stat", vars), Some("state"));
        assert_eq!(closest("count", vars), None);
        assert_eq!(
            closest("result", vars),
            None,
            "an exact match is no suggestion"
        );
    }
}
