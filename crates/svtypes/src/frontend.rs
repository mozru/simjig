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
    let short = short_version(version).ok_or_else(|| Error::UnsupportedVersion {
        version: version.to_owned(),
    })?;
    let json_err = |what: &str, e: serde_json::Error| Error::Json {
        path: "/".into(),
        source_loc: None,
        version: short.clone(),
        message: format!("{what} is not valid JSON: {e}"),
    };
    let tree: Value = serde_json::from_str(tree_json).map_err(|e| json_err(".tree.json", e))?;
    let meta: Value =
        serde_json::from_str(meta_json).map_err(|e| json_err(".tree.meta.json", e))?;
    match dialect(&short) {
        Some(Dialect::V5_05x) => v5_05x::decode(&tree, &meta, &short),
        None => Err(Error::UnsupportedVersion {
            version: version.to_owned(),
        }),
    }
}

/// The dialects this crate decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// Verilator 5.050 to 5.059.
    V5_05x,
}

fn dialect(short: &str) -> Option<Dialect> {
    let (major, minor) = short.split_once('.')?;
    match (major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?) {
        (5, 50..=59) => Some(Dialect::V5_05x),
        _ => None,
    }
}

/// `5.052` from `Verilator 5.052 2026-09-05 rev ...` or from `5.052` itself.
fn short_version(version: &str) -> Option<String> {
    version
        .split_whitespace()
        .find(|tok| {
            tok.split_once('.').is_some_and(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())
            })
        })
        .map(str::to_owned)
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
            assert_eq!(
                short_version(text).as_deref().and_then(dialect),
                expect,
                "{text}"
            );
        }
    }

    #[test]
    fn unknown_version_is_an_error() {
        let e = parse("{}", "{}", "Verilator 6.000").unwrap_err();
        assert!(matches!(e, Error::UnsupportedVersion { .. }), "{e}");
    }
}
