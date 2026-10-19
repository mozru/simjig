//! Finding the Cargo target directory that holds a path Cargo made: a build script's
//! `OUT_DIR` (simjig-build's model cache goes in `<target>/simjig-cache`) or a test binary
//! (simjig's run directories go in `<target>/simjig/runs`). One place for how simjig
//! recognises a target directory, so the two cannot drift apart.
//!
//! The rule:
//! 1. the deeper of two candidates, when either exists: the nearest ancestor (at most
//!    [`MAX_LEVELS`] up) holding the `CACHEDIR.TAG` Cargo writes at the root of a target
//!    directory it creates, and an absolute `CARGO_TARGET_DIR` that holds the path (the
//!    variable may come from another invocation than the one that built the path, so it
//!    must hold it). Both are ancestors of the path, so the deeper is the one nearer it:
//!    a tagged `target/rust-analyzer` inside `CARGO_TARGET_DIR=target`, or an untagged
//!    `CARGO_TARGET_DIR=target/ci` (made by `mkdir -p` before Cargo ran) inside a tagged
//!    `target/`;
//! 2. else the directory Cargo's layout names ([`Made`]), with the `<triple>/` level when
//!    its name is the triple built for.
//!
//! The tag alone is not enough: Cargo writes it only when it creates the directory, so a
//! `target/` another tool made first (`--target-dir target/rust-analyzer` creates it on its
//! way) never gets one.
//!
//! Why a crate (constitution rule 5): simjig needs this at run time and simjig-build at
//! build time, and neither may depend on the other (simjig-build pulls in a C++ toolchain
//! driver; simjig is what a user's tests link). This crate has no dependencies.

use std::path::{Path, PathBuf};

/// The most levels above the path that a tagged target directory is looked for: the
/// deepest layout, `<target>/<triple>/<profile>/build/<pkg>-<hash>/out`, has the target
/// directory five levels above `out`.
pub const MAX_LEVELS: usize = 5;

/// What the path is, which fixes where Cargo's layout puts the target directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Made {
    /// A build script's `OUT_DIR`: `<target>/[<triple>/]<profile>/build/<pkg>-<hash>/out`.
    OutDir,
    /// A test or bench binary: `<target>/[<triple>/]<profile>/deps/<binary>`.
    Binary,
}

/// The Cargo target directory holding `path`, a `made` path of a build for `triple` (an
/// empty `triple` matches no `<triple>/` level), by the rule in the crate docs; `None`
/// when no rule finds one.
#[must_use]
pub fn target_dir_of(
    path: &Path,
    made: Made,
    triple: &str,
    cargo_target_dir: Option<&Path>,
) -> Option<PathBuf> {
    let tagged = path
        .ancestors()
        .skip(1)
        .take(MAX_LEVELS)
        .find(|dir| dir.join("CACHEDIR.TAG").is_file());
    let named = cargo_target_dir.filter(|d| d.is_absolute() && path.starts_with(d));
    let deepest = match (tagged, named) {
        // Both hold `path`, so one holds the other.
        (Some(t), Some(n)) => Some(if n.starts_with(t) { n } else { t }),
        (t, n) => t.or(n),
    };
    deepest
        .or_else(|| by_layout(path, made, triple))
        .map(Path::to_path_buf)
}

/// Rule 2: the target directory Cargo's layout names for `path`.
fn by_layout<'a>(path: &'a Path, made: Made, triple: &str) -> Option<&'a Path> {
    let named = |p: &Path, want: &dyn Fn(&str) -> bool| {
        p.file_name().and_then(|n| n.to_str()).is_some_and(want)
    };
    let profile = match made {
        Made::OutDir => {
            let pkg = path.parent().filter(|_| named(path, &|n| n == "out"))?;
            let build = pkg.parent().filter(|_| named(pkg, &|n| n.contains('-')))?;
            build.parent().filter(|_| named(build, &|n| n == "build"))?
        }
        Made::Binary => {
            let deps = path.parent().filter(|d| named(d, &|n| n == "deps"))?;
            deps.parent()?
        }
    };
    let above = profile.parent()?;
    match above.parent() {
        Some(target) if !triple.is_empty() && named(above, &|n| n == triple) => Some(target),
        _ => Some(above),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const TRIPLE: &str = "aarch64-apple-darwin";

    /// A fresh directory under the system temp directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("simjig-target-dir-{name}-{}", std::process::id()));
            let _ignored = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ignored = fs::remove_dir_all(&self.0);
        }
    }

    fn tag(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), "Signature: x").unwrap();
    }

    /// Both layouts, with and without the `<triple>/` level, without any tag.
    #[test]
    fn the_layout_names_the_target_dir_of_an_out_dir_and_a_binary() {
        let s = Scratch::new("layout");
        let target = s.0.join("target");
        for (path, made) in [
            ("debug/build/foo-0123/out", Made::OutDir),
            ("aarch64-apple-darwin/release/build/foo-1/out", Made::OutDir),
            ("debug/deps/t-0123456789abcdef", Made::Binary),
            ("aarch64-apple-darwin/release/deps/t-0123", Made::Binary),
        ] {
            let found = target_dir_of(&target.join(path), made, TRIPLE, None);
            assert_eq!(found, Some(target.clone()), "{path}");
        }
        // Another triple's directory is not this build's `<triple>/` level; nor is any
        // with an empty triple.
        let cross = target.join("aarch64-apple-darwin/release/deps/t-0123");
        let other = Some(target.join("aarch64-apple-darwin"));
        assert_eq!(
            target_dir_of(&cross, Made::Binary, "x86_64-unknown-linux-gnu", None),
            other
        );
        assert_eq!(target_dir_of(&cross, Made::Binary, "", None), other);
        // Not Cargo's layout.
        for (path, made) in [
            ("debug/build/out", Made::OutDir),
            ("bin/tool", Made::Binary),
        ] {
            assert_eq!(
                target_dir_of(&s.0.join(path), made, TRIPLE, None),
                None,
                "{path}"
            );
        }
    }

    /// `CARGO_TARGET_DIR` counts only when absolute and holding the path; a tag below it
    /// wins (a nested target directory, `target/rust-analyzer`).
    #[test]
    fn a_tag_below_wins_then_a_cargo_target_dir_holding_the_path() {
        let s = Scratch::new("tag");
        let target = s.0.join("target");
        let plain = target.join("debug/deps/t-0123");
        let ctd = |dir: &Path| target_dir_of(&plain, Made::Binary, TRIPLE, Some(dir));
        assert_eq!(ctd(&target), Some(target.clone()));
        assert_eq!(ctd(Path::new("target")), Some(target.clone()), "relative");
        assert_eq!(
            ctd(&s.0.join("elsewhere")),
            Some(target.clone()),
            "not holding it"
        );
        let nested = target.join("ra");
        tag(&nested);
        let out = nested.join("debug/build/foo-1/out");
        let found = target_dir_of(&out, Made::OutDir, TRIPLE, Some(&target));
        assert_eq!(found, Some(nested));
    }

    /// An absolute `CARGO_TARGET_DIR` made before Cargo ran (`mkdir -p`, so untagged) is
    /// the target directory even with a tagged one above it, for simjig-build's cache and
    /// simjig's run directories alike.
    #[test]
    fn an_untagged_cargo_target_dir_beats_a_tag_above_it() {
        let s = Scratch::new("above");
        let outer = s.0.join("target");
        tag(&outer);
        let inner = outer.join("ci");
        let exe = inner.join("debug/deps/t-0123");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let found = target_dir_of(&exe, Made::Binary, TRIPLE, Some(&inner));
        assert_eq!(found, Some(inner));
    }

    /// The tag is looked for no further than the deepest layout reaches.
    #[test]
    fn a_tag_more_than_max_levels_up_is_not_looked_for() {
        let s = Scratch::new("far");
        tag(&s.0);
        let deep = s.0.join("a/b/c/d/e/f");
        assert_eq!(target_dir_of(&deep, Made::OutDir, TRIPLE, None), None);
        let near = s.0.join("a/b/c/d/e");
        assert_eq!(
            target_dir_of(&near, Made::OutDir, TRIPLE, None),
            Some(s.0.clone())
        );
    }
}
