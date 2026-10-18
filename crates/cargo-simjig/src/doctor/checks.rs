//! The checks: pure decisions from [`Facts`] to [`Finding`]s.
//!
//! Each function decides one [`Check`]. None of them runs a command; the FST check takes a
//! file-existence function so that tests can fake the include directory.

use std::path::{Path, PathBuf};

use svtypes::toolchain::{self, Verdict};
use svtypes::version::supported_list;

use super::facts::{DEFAULT_CXX, Facts, Probe};
use super::{Check, Finding};

/// The fix for a missing or unsuitable tool: install it; the simjig repository pins every
/// one in its pixi environment, but a user's project may get them elsewhere.
fn install(what: &str) -> String {
    format!("install {what} (in the simjig repo: `pixi install --locked`)")
}

/// The fix for a missing or unsupported Verilator.
fn install_verilator() -> String {
    install(&format!("Verilator {}", supported_list()))
}

/// FST tracing sources, relative to the Verilator include directory. Verilator 5.052
/// compiles these into every model built with `--trace-fst`.
const FST_SOURCES: &[&str] = &[
    "verilated_fst_c.h",
    "verilated_fst_c.cpp",
    "fstcpp/fstcpp_writer.h",
    "fstcpp/fstcpp_writer.cpp",
];

/// Runs every check in [`Check`] order.
pub(super) fn run_all(facts: &Facts, exists: &dyn Fn(&Path) -> bool) -> Vec<Finding> {
    vec![
        verilator(facts),
        verilator_root(facts),
        fst(facts, exists),
        cxx(facts),
        ccache(facts),
        nextest(facts),
        paths(facts),
        environment(facts),
    ]
}

fn verilator(facts: &Facts) -> Finding {
    let check = Check::Verilator;
    let Some(path) = &facts.verilator else {
        let searched: Vec<String> = facts
            .verilator_searched
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let summary = format!(
            "verilator not found (SIMJIG_VERILATOR, then $VERILATOR_ROOT/bin, then PATH); \
             searched: {}",
            searched.join(", ")
        );
        // The build looks where VERILATOR_ROOT points and nowhere else, so a stale one is
        // the cause whenever it is set (and SIMJIG_VERILATOR is not, which would win).
        let fix = if facts.env_root.is_some() && facts.verilator_searched.len() == 1 {
            "unset VERILATOR_ROOT, or point it at a Verilator installation".to_owned()
        } else {
            install_verilator()
        };
        return Finding::fail(check, summary, fix);
    };
    let banner = match &facts.verilator_version {
        Probe::Output(text) => text,
        Probe::NotFound => {
            return Finding::fail(
                check,
                format!("{} cannot run", path.display()),
                install_verilator(),
            );
        }
        Probe::Failed(error) => {
            let summary = format!("`{} --version` failed: {error}", path.display());
            return Finding::fail(check, summary, install_verilator());
        }
    };
    let at = path.display();
    // The build's rule (svtypes::toolchain::judge), so the doctor and the build agree.
    match toolchain::judge(banner, facts.allow_untested) {
        // The build resolves Verilator when its build script runs and tracks
        // SIMJIG_VERILATOR and VERILATOR_ROOT, not PATH (BuildEnv::TRACKED_VARS).
        Verdict::Supported(version) => Finding::ok(
            check,
            format!(
                "Verilator {version} at {at}; builds keep the Verilator their build script \
                 found (switching PATH alone does not rebuild; setting SIMJIG_VERILATOR does)"
            ),
        ),
        Verdict::Untested(version) => {
            let summary = format!(
                "Verilator {version} at {at} is untested (supported: {}); allowed by SIMJIG_UNTESTED_VERILATOR=1",
                supported_list()
            );
            Finding::warn(check, summary, install_verilator())
        }
        Verdict::Refused(Some(version)) => {
            let summary = format!(
                "Verilator {version} at {at} is not supported (supported: {})",
                supported_list()
            );
            Finding::fail(check, summary, install_verilator())
        }
        Verdict::Refused(None) => {
            let line = banner.lines().next().unwrap_or("");
            Finding::fail(
                check,
                format!("cannot read a version from `{line}`"),
                install_verilator(),
            )
        }
    }
}

/// The root the `verilator` binary uses, when it could be asked.
fn binary_root(facts: &Facts) -> Option<PathBuf> {
    facts
        .verilator_root
        .first_line()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
}

fn verilator_root(facts: &Facts) -> Finding {
    let check = Check::VerilatorRoot;
    let Some(root) = binary_root(facts) else {
        return Finding::warn(
            check,
            "cannot check: `verilator --getenv VERILATOR_ROOT` did not run",
            install_verilator(),
        );
    };
    match &facts.env_root {
        None => Finding::ok(
            check,
            format!("VERILATOR_ROOT unset; verilator uses {}", root.display()),
        ),
        Some(env) if env.canonical.as_deref() == Some(root.as_path()) => {
            Finding::ok(check, format!("VERILATOR_ROOT matches {}", root.display()))
        }
        Some(env) => Finding::fail(
            check,
            format!(
                "VERILATOR_ROOT={} but verilator uses {}; Verilator refuses to run",
                env.raw,
                root.display()
            ),
            "unset VERILATOR_ROOT",
        ),
    }
}

fn fst(facts: &Facts, exists: &dyn Fn(&Path) -> bool) -> Finding {
    let check = Check::Fst;
    let Some(root) = binary_root(facts) else {
        return Finding::warn(
            check,
            "cannot check: Verilator root unknown",
            install_verilator(),
        );
    };
    let include = root.join("include");
    let missing: Vec<&str> = FST_SOURCES
        .iter()
        .copied()
        .filter(|file| !exists(&include.join(file)))
        .collect();
    if missing.is_empty() {
        Finding::ok(
            check,
            format!("FST tracing sources in {}", include.display()),
        )
    } else {
        let summary = format!("missing from {}: {}", include.display(), missing.join(", "));
        Finding::fail(check, summary, install_verilator())
    }
}

fn cxx(facts: &Facts) -> Finding {
    let check = Check::Cxx;
    let program = facts.cxx.as_deref().unwrap_or(DEFAULT_CXX);
    match (&facts.cxx, facts.cxx_version.first_line()) {
        (Some(cxx), Some(version)) => Finding::ok(check, format!("CXX={cxx}: {version}")),
        (None, Some(version)) => Finding::warn(
            check,
            format!("CXX unset; builds use `{DEFAULT_CXX}`: {version}"),
            install("a C++ compiler, and set CXX to it"),
        ),
        (_, None) => {
            let why = match &facts.cxx_version {
                Probe::Failed(error) => format!("failed: {error}"),
                Probe::NotFound | Probe::Output(_) => "not found".to_owned(),
            };
            Finding::fail(
                check,
                format!("C++ compiler `{program}` {why}"),
                install("a C++ compiler, and set CXX to it"),
            )
        }
    }
}

fn ccache(facts: &Facts) -> Finding {
    match facts.ccache.first_line() {
        Some(version) => Finding::ok(Check::Ccache, version),
        None => Finding::warn(
            Check::Ccache,
            "ccache not found; every model rebuild recompiles all C++",
            install("ccache"),
        ),
    }
}

fn nextest(facts: &Facts) -> Finding {
    match facts.nextest.first_line() {
        Some(version) => Finding::ok(Check::Nextest, version),
        None => Finding::warn(
            Check::Nextest,
            "cargo-nextest not found; a Verilator fatal under `cargo test` kills every test in the binary",
            "cargo install --locked cargo-nextest",
        ),
    }
}

fn has_space(path: &Path) -> bool {
    path.to_string_lossy().contains(char::is_whitespace)
}

fn paths(facts: &Facts) -> Finding {
    let check = Check::Paths;
    // Verilator's launcher and generated makefiles split paths on whitespace.
    if has_space(&facts.cwd) {
        let cwd = facts.cwd.display().to_string();
        let target = cwd.replace(char::is_whitespace, "-");
        return Finding::fail(
            check,
            format!("current directory `{cwd}` contains a space; Verilator builds break"),
            format!("mv '{cwd}' '{target}'"),
        );
    }
    let install = [facts.verilator.clone(), binary_root(facts)];
    if let Some(bad) = install.iter().flatten().find(|path| has_space(path)) {
        return Finding::fail(
            check,
            format!(
                "Verilator install path `{}` contains a space",
                bad.display()
            ),
            "move the project to a path without spaces, then: pixi install --locked",
        );
    }
    Finding::ok(
        check,
        format!(
            "no spaces in {} or the Verilator install",
            facts.cwd.display()
        ),
    )
}

fn environment(facts: &Facts) -> Finding {
    let check = Check::Environment;
    if let Some(name) = &facts.pixi_environment {
        let prefix = facts.conda_prefix.as_deref().unwrap_or("unknown prefix");
        return Finding::ok(check, format!("pixi environment `{name}` at {prefix}"));
    }
    let tools = [
        ("verilator", facts.verilator.is_some()),
        ("make", facts.make.is_some()),
        ("a C++ compiler", facts.cxx_version.first_line().is_some()),
    ];
    let missing: Vec<&str> = tools
        .iter()
        .filter(|(_, found)| !found)
        .map(|(name, _)| *name)
        .collect();
    if missing.is_empty() {
        Finding::ok(
            check,
            "not in a pixi environment; verilator, make and a C++ compiler are on PATH",
        )
    } else {
        Finding::fail(
            check,
            format!(
                "not in a pixi environment, and not on PATH: {}",
                missing.join(", ")
            ),
            install("Verilator, make and a C++ compiler"),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::Status;
    use super::super::facts::EnvRoot;
    use super::*;

    const ROOT: &str = "/env/share/verilator";

    /// A machine inside a pixi environment where every check passes.
    fn healthy() -> Facts {
        Facts {
            cwd: PathBuf::from("/src/simjig"),
            verilator: Some(PathBuf::from("/env/bin/verilator")),
            verilator_searched: Vec::new(),
            verilator_version: Probe::Output(
                "Verilator 5.052 2026-09-05 rev conda-forge build".to_owned(),
            ),
            verilator_root: Probe::Output(ROOT.to_owned()),
            env_root: None,
            cxx: Some("clang++".to_owned()),
            cxx_version: Probe::Output(
                "clang version 21.1.8\nTarget: arm64-apple-darwin20.0.0".to_owned(),
            ),
            ccache: Probe::Output("ccache version 4.14.1\nFeatures: ...".to_owned()),
            nextest: Probe::Output("cargo-nextest 0.9.148 (cd1d6d546 2026-10-07)".to_owned()),
            make: Some(PathBuf::from("/env/bin/make")),
            pixi_environment: Some("default".to_owned()),
            conda_prefix: Some("/env".to_owned()),
            allow_untested: false,
        }
    }

    fn all_present(_: &Path) -> bool {
        true
    }

    /// Asserts the status and that the summary or fix mentions `needle`.
    #[track_caller]
    fn expect(finding: &Finding, status: Status, needle: &str) {
        assert_eq!(finding.status(), status, "{finding:?}");
        assert_eq!(finding.fix().is_none(), status == Status::Ok, "{finding:?}");
        let text = format!("{} {}", finding.summary(), finding.fix().unwrap_or(""));
        assert!(text.contains(needle), "{needle:?} not in {finding:?}");
    }

    #[test]
    fn healthy_machine_passes_every_check_in_order() {
        let findings = run_all(&healthy(), &all_present);
        let checks: Vec<Check> = findings.iter().map(Finding::check).collect();
        let mut sorted = checks.clone();
        sorted.sort();
        assert_eq!(checks, sorted);
        assert_eq!(checks.len(), 8);
        for finding in &findings {
            assert_eq!(finding.status(), Status::Ok, "{finding:?}");
        }
    }

    #[test]
    fn verilator_version() {
        expect(
            &verilator(&healthy()),
            Status::Ok,
            "Verilator 5.052 at /env/bin/verilator",
        );

        let mut f = healthy();
        f.verilator = None;
        f.verilator_searched = vec![PathBuf::from("/opt/v/bin/verilator")];
        expect(
            &verilator(&f),
            Status::Fail,
            "not found (SIMJIG_VERILATOR, then $VERILATOR_ROOT/bin, then PATH); searched: \
             /opt/v/bin/verilator",
        );
        f.env_root = Some(EnvRoot {
            raw: "/opt/v".to_owned(),
            canonical: None,
        });
        expect(&verilator(&f), Status::Fail, "unset VERILATOR_ROOT");

        let mut f = healthy();
        f.verilator_version = Probe::Failed("%Error: oops".to_owned());
        expect(&verilator(&f), Status::Fail, "%Error: oops");

        let mut f = healthy();
        f.verilator_version = Probe::Output("garbage".to_owned());
        expect(
            &verilator(&f),
            Status::Fail,
            "cannot read a version from `garbage`",
        );

        let mut f = healthy();
        f.verilator_version = Probe::Output("Verilator 5.053 devel rev v5.052-4".to_owned());
        expect(
            &verilator(&f),
            Status::Fail,
            "5.053 at /env/bin/verilator is not supported (supported: 5.052)",
        );
        assert_eq!(
            verilator(&f).fix(),
            Some("install Verilator 5.052 (in the simjig repo: `pixi install --locked`)")
        );
        f.allow_untested = true;
        expect(&verilator(&f), Status::Warn, "SIMJIG_UNTESTED_VERILATOR=1");
        expect(
            &verilator(&healthy()),
            Status::Ok,
            "switching PATH alone does not rebuild; setting SIMJIG_VERILATOR does",
        );
    }

    #[test]
    fn verilator_root_must_match_the_binary() {
        expect(&verilator_root(&healthy()), Status::Ok, "unset");

        let mut f = healthy();
        f.env_root = Some(EnvRoot {
            raw: "/env/bin/../share/verilator".to_owned(),
            canonical: Some(PathBuf::from(ROOT)),
        });
        expect(
            &verilator_root(&f),
            Status::Ok,
            "matches /env/share/verilator",
        );

        f.env_root = Some(EnvRoot {
            raw: "/opt/verilator".to_owned(),
            canonical: Some(PathBuf::from("/opt/verilator")),
        });
        expect(&verilator_root(&f), Status::Fail, "unset VERILATOR_ROOT");

        f.env_root = Some(EnvRoot {
            raw: "/does/not/exist".to_owned(),
            canonical: None,
        });
        expect(
            &verilator_root(&f),
            Status::Fail,
            "VERILATOR_ROOT=/does/not/exist",
        );

        let mut f = healthy();
        f.verilator_root = Probe::NotFound;
        expect(&verilator_root(&f), Status::Warn, "cannot check");
    }

    #[test]
    fn fst_sources_must_all_exist() {
        let include = Path::new(ROOT).join("include");
        let present: BTreeSet<PathBuf> = FST_SOURCES.iter().map(|f| include.join(f)).collect();
        let exists = |path: &Path| present.contains(path);
        expect(
            &fst(&healthy(), &exists),
            Status::Ok,
            "/env/share/verilator/include",
        );

        let exists = |path: &Path| present.contains(path) && !path.ends_with("fstcpp_writer.h");
        expect(
            &fst(&healthy(), &exists),
            Status::Fail,
            "fstcpp/fstcpp_writer.h",
        );

        let mut f = healthy();
        f.verilator_root = Probe::Failed("boom".to_owned());
        expect(&fst(&f, &all_present), Status::Warn, "cannot check");
    }

    #[test]
    fn cxx_reports_compiler_and_version() {
        expect(
            &cxx(&healthy()),
            Status::Ok,
            "CXX=clang++: clang version 21.1.8",
        );

        let mut f = healthy();
        f.cxx = None;
        expect(
            &cxx(&f),
            Status::Warn,
            "CXX unset; builds use `c++`: clang version 21.1.8",
        );

        let mut f = healthy();
        f.cxx_version = Probe::NotFound;
        expect(&cxx(&f), Status::Fail, "`clang++` not found");

        f.cxx_version = Probe::Failed("bad flag".to_owned());
        expect(&cxx(&f), Status::Fail, "failed: bad flag");
    }

    #[test]
    fn ccache_and_nextest_are_warnings() {
        expect(&ccache(&healthy()), Status::Ok, "ccache version 4.14.1");
        expect(&nextest(&healthy()), Status::Ok, "cargo-nextest 0.9.148");

        let mut f = healthy();
        f.ccache = Probe::NotFound;
        f.nextest = Probe::Failed("error: no such command: `nextest`".to_owned());
        expect(
            &ccache(&f),
            Status::Warn,
            "install ccache (in the simjig repo: `pixi install --locked`)",
        );
        expect(
            &nextest(&f),
            Status::Warn,
            "cargo install --locked cargo-nextest",
        );
    }

    #[test]
    fn paths_must_not_contain_spaces() {
        expect(&paths(&healthy()), Status::Ok, "/src/simjig");

        let mut f = healthy();
        f.cwd = PathBuf::from("/Users/me/Library/Application Support/simjig");
        expect(
            &paths(&f),
            Status::Fail,
            "mv '/Users/me/Library/Application Support/simjig' '/Users/me/Library/Application-Support/simjig'",
        );

        let mut f = healthy();
        f.verilator_root = Probe::Output("/opt/my tools/share/verilator".to_owned());
        expect(&paths(&f), Status::Fail, "`/opt/my tools/share/verilator`");

        let mut f = healthy();
        f.verilator = Some(PathBuf::from("/opt/my\ttools/bin/verilator"));
        expect(&paths(&f), Status::Fail, "install path");
    }

    #[test]
    fn environment_is_pixi_or_path() {
        expect(
            &environment(&healthy()),
            Status::Ok,
            "pixi environment `default` at /env",
        );

        let mut f = healthy();
        f.pixi_environment = None;
        expect(&environment(&f), Status::Ok, "are on PATH");

        f.make = None;
        f.verilator = None;
        expect(
            &environment(&f),
            Status::Fail,
            "not on PATH: verilator, make",
        );
    }
}
