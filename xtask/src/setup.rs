//! `cargo xtask setup`: installs every pinned tool a developer needs.
//!
//! Three pins make the environment reproducible: `rust-toolchain.toml` (rustup installs
//! it on the first cargo command), `pixi.lock` (Verilator, the C++ compiler, ccache,
//! make), and [`DEV_TOOLS`] below (the cargo tools `cargo xtask ci` runs).
//!
//! Setup is idempotent. A cargo tool is reinstalled only when the binary found on `PATH`
//! does not report its pinned version, and after installing, setup checks the version
//! again, so a stale copy earlier on `PATH` is an error instead of a silent mismatch.

use std::process::Command;

use crate::run;

/// A cargo tool installed with `cargo install --locked --version <version> <krate>`.
#[derive(Debug)]
pub(crate) struct DevTool {
    krate: &'static str,
    version: &'static str,
    /// The installed program and the arguments that make it print its version.
    program: &'static str,
    version_args: &'static [&'static str],
}

/// The pinned cargo tools: the single place their versions are written.
pub(crate) const DEV_TOOLS: &[DevTool] = &[
    DevTool {
        krate: "cargo-nextest",
        version: "0.9.148",
        program: "cargo-nextest",
        version_args: &["nextest", "--version"],
    },
    DevTool {
        krate: "cargo-deny",
        version: "0.20.2",
        program: "cargo-deny",
        version_args: &["--version"],
    },
    DevTool {
        krate: "cargo-machete",
        version: "0.9.2",
        program: "cargo-machete",
        version_args: &["--version"],
    },
    DevTool {
        krate: "typos-cli",
        version: "1.51.1",
        program: "typos",
        version_args: &["--version"],
    },
];

/// The version in a `--version` banner: the first word of the first line made of
/// dot-separated numbers, such as `0.9.148` in `cargo-nextest 0.9.148 (cd1d6d546)`.
fn banner_version(banner: &str) -> Option<&str> {
    let is_version = |word: &str| {
        word.split('.').count() >= 2
            && word
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    };
    banner
        .lines()
        .next()?
        .split_whitespace()
        .find(|w| is_version(w))
}

/// Whether `banner` (the tool's `--version` output, `None` if it did not run) shows a
/// version other than the pin.
fn needs_install(tool: &DevTool, banner: Option<&str>) -> bool {
    banner.and_then(banner_version) != Some(tool.version)
}

/// The tool's `--version` output, or `None` when it is missing or fails.
fn installed_banner(tool: &DevTool) -> Option<String> {
    let output = Command::new(tool.program)
        .args(tool.version_args)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn install(tool: &DevTool) -> Result<(), String> {
    if !needs_install(tool, installed_banner(tool).as_deref()) {
        eprintln!("==> {} {} already installed", tool.krate, tool.version);
        return Ok(());
    }
    run(
        "cargo",
        &["install", "--locked", "--version", tool.version, tool.krate],
        &[],
    )?;
    let banner = installed_banner(tool);
    if needs_install(tool, banner.as_deref()) {
        let found = banner.as_deref().and_then(banner_version).unwrap_or("none");
        return Err(format!(
            "installed {} {}, but `{}` on PATH reports version {found}; \
             put ~/.cargo/bin first on PATH",
            tool.krate, tool.version, tool.program
        ));
    }
    Ok(())
}

/// Installs the pixi environment and every [`DEV_TOOLS`] entry.
pub(crate) fn setup() -> Result<(), String> {
    run("pixi", &["install", "--locked"], &[])?;
    DEV_TOOLS.iter().try_for_each(install)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_from_tool_banners() {
        let check = |banner: &str, expect: Option<&str>| {
            assert_eq!(banner_version(banner), expect, "{banner:?}");
        };
        check(
            "cargo-nextest 0.9.148 (cd1d6d546 2026-10-07)\nrelease: 0.9.148",
            Some("0.9.148"),
        );
        check("cargo-deny 0.20.2\n", Some("0.20.2"));
        check("0.9.2", Some("0.9.2"));
        check("typos-cli 1.51.1", Some("1.51.1"));
        check("", None);
        check("tool v1.2.3", None);
        check("tool 1", None);
        check("tool 1..2", None);
        check("tool\n1.2.3", None);
    }

    #[test]
    fn installs_only_when_the_version_differs() {
        let nextest = &DEV_TOOLS[0];
        assert!(!needs_install(nextest, Some("cargo-nextest 0.9.148 (abc)")));
        assert!(needs_install(nextest, Some("cargo-nextest 0.9.147 (abc)")));
        assert!(needs_install(nextest, Some("cargo-nextest 0.9.1480")));
        assert!(needs_install(nextest, Some("garbage")));
        assert!(needs_install(nextest, None));
    }

    #[test]
    fn every_pin_is_an_exact_version() {
        for tool in DEV_TOOLS {
            assert_eq!(banner_version(tool.version), Some(tool.version), "{tool:?}");
        }
    }
}
