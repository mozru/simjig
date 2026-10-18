//! Verilator versions: the one parser and the one supported-version table.
//!
//! Verilator numbers releases `MAJOR.MINOR` with a three-digit minor (`5.052`); conda-forge
//! writes the same release as `5.52`. Both parse to the same [`VerilatorVersion`], and it
//! always displays in Verilator's own spelling.
//!
//! simjig decodes Verilator's output (the `--json-only` AST, the header port macros,
//! `_classes.mk`) and links against its runtime, and those change between releases, so
//! only the versions in [`SUPPORTED`] are accepted. Every build-time crate (the JSON front
//! end, `simjig-build`, `cargo simjig doctor`) reads this table; adding a version means
//! adding fixtures for it.

use std::fmt;
use std::str::FromStr;

/// A Verilator release number, such as `5.052`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VerilatorVersion {
    major: u32,
    minor: u32,
}

/// The Verilator versions simjig is tested against. Keep it in step with `pixi.toml`.
pub const SUPPORTED: &[VerilatorVersion] = &[VerilatorVersion::new(5, 52)];

impl VerilatorVersion {
    /// Makes the version `major.minor`; `new(5, 52)` is `5.052`.
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// The major release number (`5` for `5.052`).
    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    /// The minor release number (`52` for `5.052`).
    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Parses the first line of `verilator --version`, such as
    /// `Verilator 5.052 2026-09-05 rev conda-forge build`.
    ///
    /// Returns `None` when the text is not a Verilator version banner. A development
    /// build's `5.053-devel` is not a release number, so it returns `None` too.
    #[must_use]
    pub fn from_banner(banner: &str) -> Option<Self> {
        let mut words = banner.lines().next()?.split_whitespace();
        if words.next()? != "Verilator" {
            return None;
        }
        words.next()?.parse().ok()
    }

    /// Whether this version is in [`SUPPORTED`].
    #[must_use]
    pub fn is_supported(self) -> bool {
        SUPPORTED.contains(&self)
    }
}

/// [`SUPPORTED`] as a comma-separated list, for messages: `5.052`.
#[must_use]
pub fn supported_list() -> String {
    SUPPORTED
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for VerilatorVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:03}", self.major, self.minor)
    }
}

/// The error for text that is not `MAJOR.MINOR` with decimal numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVersionError {
    text: String,
}

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a Verilator version (expected MAJOR.MINOR, e.g. 5.052)",
            self.text
        )
    }
}

impl std::error::Error for ParseVersionError {}

impl FromStr for VerilatorVersion {
    type Err = ParseVersionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = || ParseVersionError {
            text: text.to_owned(),
        };
        let (major, minor) = text.split_once('.').ok_or_else(error)?;
        let number = |part: &str| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(error());
            }
            part.parse::<u32>().map_err(|_overflow| error())
        };
        Ok(Self::new(number(major)?, number(minor)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verilator_and_conda_spellings() {
        for text in ["5.052", "5.52", "05.052"] {
            assert_eq!(text.parse(), Ok(VerilatorVersion::new(5, 52)), "{text}");
        }
        assert_eq!("4.228".parse(), Ok(VerilatorVersion::new(4, 228)));
    }

    #[test]
    fn rejects_malformed_versions() {
        for text in [
            "",
            "5",
            "5.",
            ".052",
            "5.052.1",
            "v5.052",
            "5.05x",
            "5.-1",
            "+5.1",
            "5.053-devel",
        ] {
            assert!(text.parse::<VerilatorVersion>().is_err(), "{text}");
        }
        assert!("99999999999.1".parse::<VerilatorVersion>().is_err());
    }

    #[test]
    fn displays_three_digit_minor() {
        assert_eq!(VerilatorVersion::new(5, 52).to_string(), "5.052");
        assert_eq!(VerilatorVersion::new(5, 2).to_string(), "5.002");
        assert_eq!(VerilatorVersion::new(4, 228).to_string(), "4.228");
    }

    #[test]
    fn reads_version_banners() {
        let check = |banner: &str, expect: Option<VerilatorVersion>| {
            assert_eq!(VerilatorVersion::from_banner(banner), expect, "{banner:?}");
        };
        let v5_052 = Some(VerilatorVersion::new(5, 52));
        check("Verilator 5.052 2026-09-05 rev conda-forge build\n", v5_052);
        check("Verilator 5.052 2026-09-05 rev v5.052\nsecond line", v5_052);
        check(
            "Verilator 5.053 devel rev v5.052-40-g1234",
            Some(VerilatorVersion::new(5, 53)),
        );
        check("Verilator 5.053-devel rev v5.052-12-g1", None);
        check("", None);
        check("verilator 5.052", None);
        check("Verilator", None);
        check("Verilator devel", None);
        check("Icarus Verilog version 12.0", None);
    }

    #[test]
    fn supported_table_is_the_pinned_release() {
        assert!(VerilatorVersion::new(5, 52).is_supported());
        assert!(!VerilatorVersion::new(5, 53).is_supported());
        assert!(!VerilatorVersion::new(5, 51).is_supported());
        assert_eq!(supported_list(), "5.052");
    }
}
