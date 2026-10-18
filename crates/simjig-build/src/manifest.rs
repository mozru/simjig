//! Dependency manifests: the files one run read (a C++ compile's `-MD` list, or what
//! Verilator read), each with the digest of its contents and its file status, so the
//! next build can tell whether they changed without reading them all.
//!
//! The full keys of the two-step caches (`cache`, `archive`, `front`) cover every
//! dependency's contents. Re-reading them on every build costs: one model's compile reads
//! more than a thousand headers (most of them the toolchain's). So, as ccache's direct
//! mode does, a dependency whose status (size, modification and change times, inode and
//! device) is the one recorded counts as unchanged, and only the others are read and
//! hashed again.
//!
//! Inputs edited while a run reads them. The dependencies are known, and hashed, only
//! after the tool exits; an edit during the run would store the output of the old
//! contents under the key of the new ones, a hit forever after. So, as ccache does, a run
//! takes a [`Snapshot`] before it starts: the time (by the filesystem's clock) and the
//! digests of the inputs a previous run listed. [`Manifest::record`] then refuses
//! ([`Recorded::Changed`]) when a dependency's modification or change time is not
//! before the start, or its digest is not the one the snapshot saw; the caller uses the
//! output for that build only and does not publish it, and the next build runs again. A
//! dependency first listed by this run is judged by its times alone. A file whose time
//! is in the future (clock skew) is never cached, which the build's warning says.
//!
//! Invariant: a status is recorded only when it can be trusted, that is when both of the
//! file's times are older than the run that read it by more than [`TRUST_MARGIN`]. A
//! file written just before the run may change again within the filesystem's timestamp
//! granularity without its status changing; such a file is hashed on every build.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::hash::KeyHasher;

/// How much older than the run a file's times must be for its status to be trusted:
/// more than the coarsest timestamp granularity in use (FAT's 2 s).
const TRUST_MARGIN: Duration = Duration::from_secs(3);

/// The first line of a manifest file; any other text is not a manifest.
const HEADER: &str = "simjig-manifest-v1";

/// The files one run read, as recorded after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    /// [`digest`] of the contents when the manifest was recorded.
    digest: String,
    /// The file's status then, when it can be trusted (see the module docs).
    stat: Option<Stat>,
}

/// What identifies one version of a file without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
    ino: u64,
    dev: u64,
}

impl Stat {
    /// The status of `path`, and the newer of its two times.
    #[cfg(unix)]
    fn of(path: &Path) -> Option<(Option<Self>, SystemTime)> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(path).ok()?;
        let ns = |secs: i64, nanos: i64| i128::from(secs) * 1_000_000_000 + i128::from(nanos);
        let stat = Self {
            size: m.size(),
            mtime_ns: ns(m.mtime(), m.mtime_nsec()),
            ctime_ns: ns(m.ctime(), m.ctime_nsec()),
            ino: m.ino(),
            dev: m.dev(),
        };
        let newest = stat.mtime_ns.max(stat.ctime_ns);
        // A time before 1970 is no later than any run.
        let newest = u64::try_from(newest).unwrap_or(0);
        Some((
            Some(stat),
            SystemTime::UNIX_EPOCH + Duration::from_nanos(newest),
        ))
    }

    /// Elsewhere there is no status to trust (every dependency is hashed on every build),
    /// and the modification time is the only one.
    #[cfg(not(unix))]
    fn of(path: &Path) -> Option<(Option<Self>, SystemTime)> {
        Some((None, std::fs::metadata(path).ok()?.modified().ok()?))
    }

    fn text(self) -> String {
        format!(
            "{},{},{},{},{}",
            self.size, self.mtime_ns, self.ctime_ns, self.ino, self.dev
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut f = text.split(',');
        let stat = Self {
            size: f.next()?.parse().ok()?,
            mtime_ns: f.next()?.parse().ok()?,
            ctime_ns: f.next()?.parse().ok()?,
            ino: f.next()?.parse().ok()?,
            dev: f.next()?.parse().ok()?,
        };
        f.next().is_none().then_some(stat)
    }
}

/// The digest of a file's contents; `None` when it cannot be read.
fn digest(path: &Path) -> Option<String> {
    let contents = std::fs::read(path).ok()?;
    let mut h = KeyHasher::new("simjig-file-v1");
    h.field(&contents);
    Some(h.finish())
}

impl Manifest {
    /// Hashes every file of `deps` now, after a run that took `snapshot` before it
    /// started; refuses when one changed meanwhile (see the module docs) or can no
    /// longer be read.
    pub(crate) fn record(deps: &[PathBuf], snapshot: &Snapshot) -> Recorded {
        let trusted_before = snapshot.started.checked_sub(TRUST_MARGIN);
        let mut entries = Vec::with_capacity(deps.len());
        for path in deps {
            let changed = || Recorded::Changed(path.clone());
            let Some(digest) = digest(path) else {
                return changed();
            };
            // The times after the contents: an edit after reading them shows here.
            let Some((stat, newest)) = Stat::of(path) else {
                return changed();
            };
            if newest >= snapshot.started || snapshot.saw_other(path, &digest) {
                return changed();
            }
            entries.push(Entry {
                path: path.clone(),
                digest,
                stat: stat.filter(|_| trusted_before.is_some_and(|t| newest < t)),
            });
        }
        Recorded::Stable(Self { entries })
    }

    /// Reads a manifest file; `None` when there is none or it is not one.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(path).ok()?)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let entries = lines
            .map(|line| {
                let mut f = line.splitn(3, '\t');
                let digest = f.next()?.to_owned();
                let stat = match f.next()? {
                    "-" => None,
                    s => Some(Stat::parse(s)?),
                };
                Some(Entry {
                    path: PathBuf::from(f.next()?),
                    digest,
                    stat,
                })
            })
            .collect::<Option<_>>()?;
        Some(Self { entries })
    }

    /// The text [`Manifest::read`] reads back.
    pub(crate) fn text(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for e in &self.entries {
            let stat = e.stat.map_or_else(|| "-".to_owned(), Stat::text);
            for field in [
                e.digest.as_str(),
                "\t",
                &stat,
                "\t",
                &e.path.display().to_string(),
                "\n",
            ] {
                out.push_str(field);
            }
        }
        out
    }

    /// The files, in recorded order.
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.entries.iter().map(|e| e.path.clone()).collect()
    }

    /// The key in `domain` over `pre` and the contents as recorded.
    pub(crate) fn recorded_key(&self, domain: &str, pre: &str) -> String {
        key(
            domain,
            pre,
            self.entries.iter().map(|e| (&e.path, e.digest.clone())),
        )
    }

    /// The files' current digests: the recorded one for a file whose trusted status is
    /// unchanged, a new one for every other. `None` when a file can no longer be read
    /// (no entry can be valid).
    pub(crate) fn current(&self) -> Option<Digests> {
        self.entries
            .iter()
            .map(|e| {
                let same = e.stat.is_some() && Stat::of(&e.path).and_then(|(s, _)| s) == e.stat;
                let digest = if same {
                    e.digest.clone()
                } else {
                    digest(&e.path)?
                };
                Some((e.path.clone(), digest))
            })
            .collect::<Option<_>>()
            .map(Digests)
    }
}

/// What [`Manifest::record`] found.
#[derive(Debug)]
pub(crate) enum Recorded {
    /// Every dependency is as it was when the run started.
    Stable(Manifest),
    /// This dependency changed while the run read it, or vanished: the output serves
    /// this build only.
    Changed(PathBuf),
}

/// The `cargo::warning` for an output left out of the cache because `path` changed
/// while `run` ("verilating", "the front end ran").
pub(crate) fn changed_warning(dut: &str, run: &str, path: &Path) -> String {
    format!(
        "cargo::warning=simjig: DUT `{dut}`: `{}` changed while {run} (or its time is in \
         the future); this build uses the result, which is not cached, so the next build \
         does it again",
        path.display()
    )
}

/// Files and their digests, as [`Manifest::current`] found them.
#[derive(Debug, Default)]
pub(crate) struct Digests(Vec<(PathBuf, String)>);

impl Digests {
    /// The key in `domain` over `pre` and these digests.
    pub(crate) fn key(&self, domain: &str, pre: &str) -> String {
        key(domain, pre, self.0.iter().map(|(p, d)| (p, d.clone())))
    }
}

/// What a run's inputs were when it started (see the module docs).
#[derive(Debug)]
pub(crate) struct Snapshot {
    /// The start, by the filesystem's clock.
    started: SystemTime,
    /// The digests of the inputs a previous run listed, sorted by path.
    before: Vec<(PathBuf, String)>,
}

impl Snapshot {
    /// Starts a run. `work` is a directory the caller has just created: its time is the
    /// filesystem's clock, which may lag the system's (Linux stamps files with a clock
    /// that ticks every few milliseconds), and the earlier of the two is the start.
    /// `known` are the current digests of a previous run's dependencies, if any.
    pub(crate) fn start(work: &Path, known: Option<Digests>) -> Self {
        let now = SystemTime::now();
        let fs_now = Stat::of(work).map_or(now, |(_, t)| t);
        let mut before = known.unwrap_or_default().0;
        before.sort();
        Self {
            started: now.min(fs_now),
            before,
        }
    }

    /// Whether the snapshot saw `path` with another digest.
    fn saw_other(&self, path: &Path, digest: &str) -> bool {
        self.before
            .binary_search_by(|(p, _)| p.as_path().cmp(path))
            .is_ok_and(|i| self.before[i].1 != digest)
    }
}

fn key<'a>(domain: &str, pre: &str, deps: impl Iterator<Item = (&'a PathBuf, String)>) -> String {
    let mut h = KeyHasher::new(domain);
    h.str(pre);
    for (path, digest) in deps {
        h.path(path).str(&digest);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn snapshot_at(started: SystemTime, known: Option<Digests>) -> Snapshot {
        let mut before = known.unwrap_or_default().0;
        before.sort();
        Snapshot { started, before }
    }

    /// A run that started a minute from now: every file written before is trusted.
    fn later() -> Snapshot {
        snapshot_at(SystemTime::now() + Duration::from_secs(60), None)
    }

    fn stable(deps: &[PathBuf], snapshot: &Snapshot) -> Manifest {
        match Manifest::record(deps, snapshot) {
            Recorded::Stable(m) => m,
            Recorded::Changed(p) => panic!("{} changed", p.display()),
        }
    }

    fn current_key(m: &Manifest, domain: &str, pre: &str) -> Option<String> {
        m.current().map(|d| d.key(domain, pre))
    }

    #[test]
    fn the_key_tracks_contents_and_a_vanished_file_is_a_miss() {
        let t = tempfile::tempdir().unwrap();
        let h = old_file(t.path(), "k.h", "#define K 1\n");
        let m = stable(std::slice::from_ref(&h), &later());
        let k1 = m.recorded_key("d", "pre");
        assert_eq!(current_key(&m, "d", "pre").unwrap(), k1);
        assert_ne!(current_key(&m, "d", "other").unwrap(), k1);
        // Same size: only the status shows the change.
        std::fs::write(&h, "#define K 2\n").unwrap();
        assert_ne!(current_key(&m, "d", "pre").unwrap(), k1);
        std::fs::remove_file(&h).unwrap();
        assert_eq!(
            current_key(&m, "d", "pre"),
            None,
            "a vanished header is a miss"
        );
    }

    /// A file whose status is the recorded one is not read again: a manifest recording a
    /// different digest for it keys on that digest.
    #[test]
    fn an_unchanged_status_skips_hashing() {
        let t = tempfile::tempdir().unwrap();
        let h = old_file(t.path(), "k.h", "#define K 1\n");
        let m = stable(std::slice::from_ref(&h), &later());
        let text = m.text().replace(&m.entries[0].digest, "recorded");
        let stale = Manifest::parse(&text).unwrap();
        let expect = key("d", "pre", std::iter::once((&h, "recorded".to_owned())));
        assert_eq!(current_key(&stale, "d", "pre").unwrap(), expect);
        // Without a status (here: no longer trusted), the file is hashed.
        let status = Stat::of(&h).unwrap().0.unwrap().text();
        let untrusted = Manifest::parse(&text.replace(&status, "-")).unwrap();
        assert_eq!(
            current_key(&untrusted, "d", "pre").unwrap(),
            m.recorded_key("d", "pre")
        );
    }

    /// A file written shortly before the run may change again with the same status, so
    /// its status is not recorded.
    #[test]
    fn a_status_just_older_than_the_run_is_not_trusted() {
        let t = tempfile::tempdir().unwrap();
        let h = old_file(t.path(), "k.h", "x");
        let soon = snapshot_at(SystemTime::now() + Duration::from_secs(1), None);
        let m = stable(std::slice::from_ref(&h), &soon);
        assert_eq!(m.entries[0].stat, None);
        assert!(stable(&[h], &later()).entries[0].stat.is_some());
    }

    /// A dependency whose time is not before the run's start changed while the run read
    /// it (or is new to it): the output must not be recorded under its contents.
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "crossing the filesystem clock's tick; a cache test, not a testbench"
    )]
    fn a_dependency_newer_than_the_run_is_changed() {
        let t = tempfile::tempdir().unwrap();
        let old = old_file(t.path(), "old.h", "o");
        // Past the filesystem clock's tick, so `old.h` is older than the start.
        std::thread::sleep(Duration::from_millis(20));
        let work = t.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let started = Snapshot::start(&work, None);
        let new = old_file(t.path(), "new.h", "n");
        let deps = [old.clone(), new.clone()];
        assert!(matches!(
            Manifest::record(&deps, &started),
            Recorded::Changed(p) if p == new
        ));
        let gone = t.path().join("gone.h");
        assert!(matches!(
            Manifest::record(&[old, gone.clone()], &later()),
            Recorded::Changed(p) if p == gone
        ));
    }

    /// An edit that keeps the times older than the run (`cp -p`, a coarse clock) still
    /// shows in the digest of an input the snapshot knew.
    #[test]
    fn a_known_dependency_with_another_digest_is_changed() {
        let t = tempfile::tempdir().unwrap();
        let h = old_file(t.path(), "k.h", "#define K 1\n");
        let m = stable(std::slice::from_ref(&h), &later());
        let snapshot = snapshot_at(SystemTime::now() + Duration::from_secs(60), m.current());
        std::fs::write(&h, "#define K 2\n").unwrap();
        assert!(matches!(
            Manifest::record(std::slice::from_ref(&h), &snapshot),
            Recorded::Changed(p) if p == h
        ));
        std::fs::write(&h, "#define K 1\n").unwrap();
        stable(&[h], &snapshot);
    }

    #[test]
    fn manifests_round_trip_and_reject_other_text() {
        let t = tempfile::tempdir().unwrap();
        let deps = [
            old_file(t.path(), "a.h", "a"),
            old_file(t.path(), "b.h", "b"),
        ];
        let m = stable(&deps, &later());
        assert_eq!(Manifest::parse(&m.text()), Some(m.clone()));
        assert_eq!(m.paths(), deps);
        assert_eq!(Manifest::parse("/a.h\n/b.h\n"), None, "an older plain list");
    }
}
