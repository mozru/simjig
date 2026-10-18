//! Front ends: one decoder per producer dialect, selected from the producer version.
//!
//! Verilator's `--json-only` output is not a stable format, so each supported release
//! range gets its own decoder module and fixture corpus. An unknown version is an error,
//! never a best-effort decode.

mod json;
mod v5_05x;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::ir::Design;
use crate::version::VerilatorVersion;

/// Decodes Verilator `--json-only` output into a [`Design`].
///
/// `tree_json` and `meta_json` are the contents of `<prefix>.tree.json` and
/// `<prefix>.tree.meta.json`; `version` is either the full `verilator --version` line or
/// just `5.052`.
///
/// # Errors
/// [`Error::UnsupportedVersion`] when no dialect matches, [`Error::Json`] when the document
/// does not parse or contains a node or field this dialect does not know.
pub fn parse(tree_json: &str, meta_json: &str, version: &str) -> Result<Design> {
    let unsupported = || Error::UnsupportedVersion {
        version: version.to_owned(),
    };
    let parsed = parse_version(version).ok_or_else(unsupported)?;
    let short = parsed.to_string();
    let json_err = |what: &str, e: serde_json::Error| Error::Json {
        path: "/".into(),
        source_loc: None,
        version: short.clone(),
        message: format!("{what} is not valid JSON: {e}"),
    };
    let tree: Value = serde_json::from_str(tree_json).map_err(|e| json_err(".tree.json", e))?;
    let meta: Value =
        serde_json::from_str(meta_json).map_err(|e| json_err(".tree.meta.json", e))?;
    match dialect(parsed) {
        Some(Dialect::V5_05x) => v5_05x::decode(&tree, &meta, &short),
        None => Err(unsupported()),
    }
}

/// The dialects this crate decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// Verilator 5.050 to 5.059.
    V5_05x,
}

fn dialect(version: VerilatorVersion) -> Option<Dialect> {
    match (version.major(), version.minor()) {
        (5, 50..=59) => Some(Dialect::V5_05x),
        _ => None,
    }
}

/// The version in a `verilator --version` banner, or in a bare `5.052`.
fn parse_version(version: &str) -> Option<VerilatorVersion> {
    VerilatorVersion::from_banner(version).or_else(|| version.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_dialect_from_version() {
        let cases = [
            (
                "Verilator 5.052 2026-09-05 rev conda-forge build",
                Some(Dialect::V5_05x),
            ),
            ("5.050", Some(Dialect::V5_05x)),
            ("5.059", Some(Dialect::V5_05x)),
            ("Verilator 5.060 devel", None),
            ("Verilator 5.049", None),
            ("Verilator 4.228", None),
            ("Verilator", None),
        ];
        for (text, expect) in cases {
            assert_eq!(parse_version(text).and_then(dialect), expect, "{text}");
        }
    }

    #[test]
    fn unknown_version_is_an_error() {
        let e = parse("{}", "{}", "Verilator 6.000").unwrap_err();
        assert!(matches!(e, Error::UnsupportedVersion { .. }), "{e}");
    }
}
