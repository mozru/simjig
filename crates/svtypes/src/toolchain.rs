//! Which Verilator simjig runs, and whether it accepts it: the one rule that the build
//! (`simjig-build`) and `cargo simjig doctor` both apply, so the doctor never approves a
//! Verilator the build would refuse, or the other way round.
//!
//! - **Locate** ([`locate`]): `SIMJIG_VERILATOR`, else `$VERILATOR_ROOT/bin/verilator`,
//!   else the first `verilator` on `PATH`. An explicit choice (either variable) that is not
//!   an executable is an error; it never falls through to `PATH`, which would build with a
//!   Verilator the user did not pick.
//! - **Accept** ([`judge`]): a version in [`SUPPORTED`](crate::version::SUPPORTED) is
//!   accepted. Any other release is refused unless `SIMJIG_UNTESTED_VERILATOR=1`, which
//!   accepts it as untested (callers warn). A banner that is not a release number (a
//!   development build) is always refused: nothing about its output is known.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::version::VerilatorVersion;

/// The variable naming an explicit `verilator` executable.
pub const SIMJIG_VERILATOR: &str = "SIMJIG_VERILATOR";
/// The variable that accepts an unsupported Verilator release as untested when `1`.
pub const UNTESTED_VAR: &str = "SIMJIG_UNTESTED_VERILATOR";

/// Where [`locate`] looks: the three inputs of the rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    /// `SIMJIG_VERILATOR`.
    pub simjig_verilator: Option<PathBuf>,
    /// `VERILATOR_ROOT`.
    pub verilator_root: Option<PathBuf>,
    /// `PATH`.
    pub path: Option<OsString>,
}

impl Search {
    /// The search the process environment describes (empty variables count as unset).
    #[must_use]
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        Self {
            simjig_verilator: var(SIMJIG_VERILATOR).map(PathBuf::from),
            verilator_root: var("VERILATOR_ROOT").map(PathBuf::from),
            path: var("PATH"),
        }
    }
}

/// No usable `verilator`; `searched` lists every candidate, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotFound {
    /// The candidates checked.
    pub searched: Vec<PathBuf>,
}

/// The Verilator executable `search` selects (see the module docs for the order).
///
/// # Errors
/// [`NotFound`] with the explicit choice alone when it is not an executable, or with every
/// `PATH` candidate otherwise.
pub fn locate(search: &Search) -> Result<PathBuf, NotFound> {
    let explicit = search.simjig_verilator.clone().or_else(|| {
        search
            .verilator_root
            .as_ref()
            .map(|r| r.join("bin/verilator"))
    });
    if let Some(exe) = explicit {
        return if is_executable(&exe) {
            Ok(exe)
        } else {
            Err(NotFound {
                searched: vec![exe],
            })
        };
    }
    let mut searched = Vec::new();
    for dir in search.path.iter().flat_map(std::env::split_paths) {
        let exe = dir.join("verilator");
        if is_executable(&exe) {
            return Ok(exe);
        }
        searched.push(exe);
    }
    Err(NotFound { searched })
}

/// Whether `path` is a file the current user may execute.
#[must_use]
pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// What [`judge`] decided about one Verilator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A supported release.
    Supported(VerilatorVersion),
    /// An unsupported release, accepted because `SIMJIG_UNTESTED_VERILATOR=1`.
    Untested(VerilatorVersion),
    /// Refused: an unsupported release without the override, or (`None`) a banner that
    /// is not a release number.
    Refused(Option<VerilatorVersion>),
}

/// Whether `SIMJIG_UNTESTED_VERILATOR`'s value accepts untested releases (exactly `1`).
#[must_use]
pub fn allows_untested(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

/// Decides on the Verilator whose `--version` first line is `banner`.
#[must_use]
pub fn judge(banner: &str, allow_untested: bool) -> Verdict {
    match VerilatorVersion::from_banner(banner) {
        Some(v) if v.is_supported() => Verdict::Supported(v),
        Some(v) if allow_untested => Verdict::Untested(v),
        other => Verdict::Refused(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn script(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A scratch directory, removed on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            // Best effort: a leftover temp directory is harmless.
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[cfg(unix)]
    #[test]
    fn locate_prefers_the_explicit_choices_then_path() {
        let dir = std::env::temp_dir().join(format!("svtypes-locate-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let t = Tmp(dir);
        let on_path = t.path().join("bin/verilator");
        let in_root = t.path().join("root/bin/verilator");
        let chosen = t.path().join("chosen");
        for p in [&on_path, &in_root, &chosen] {
            script(p);
        }
        let path =
            Some(std::env::join_paths([t.path().join("empty"), t.path().join("bin")]).unwrap());
        let mut s = Search {
            simjig_verilator: Some(chosen.clone()),
            verilator_root: Some(t.path().join("root")),
            path,
        };
        assert_eq!(locate(&s), Ok(chosen));
        s.simjig_verilator = None;
        assert_eq!(locate(&s), Ok(in_root));
        s.verilator_root = None;
        assert_eq!(locate(&s), Ok(on_path.clone()));
        // A wrong explicit choice is an error, never PATH's verilator.
        s.verilator_root = Some(t.path().join("nowhere"));
        let wrong = t.path().join("nowhere/bin/verilator");
        assert_eq!(
            locate(&s),
            Err(NotFound {
                searched: vec![wrong]
            })
        );
        s.verilator_root = None;
        s.path = Some(t.path().join("empty").into_os_string());
        assert_eq!(
            locate(&s),
            Err(NotFound {
                searched: vec![t.path().join("empty/verilator")]
            })
        );
    }

    #[test]
    fn judge_accepts_supported_and_overridden_releases_only() {
        let v = |m| VerilatorVersion::new(5, m);
        assert_eq!(
            judge("Verilator 5.052 2026-09-05", false),
            Verdict::Supported(v(52))
        );
        assert_eq!(
            judge("Verilator 5.051 2026-01-01", false),
            Verdict::Refused(Some(v(51)))
        );
        assert_eq!(
            judge("Verilator 5.051 2026-01-01", true),
            Verdict::Untested(v(51))
        );
        assert_eq!(judge("Verilator 5.053-devel", true), Verdict::Refused(None));
        assert!(allows_untested(Some(OsStr::new("1"))));
        assert!(!allows_untested(Some(OsStr::new("yes"))) && !allows_untested(None));
    }
}
