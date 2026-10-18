//! Dependency manifests: the files one run read (a C++ compile's `-MD` list, or what
//! Verilator read), each with the digest of its contents and its file status, so the
//! next build can tell whether they changed without reading them all.
//!
//! The full keys of the two-step caches (`archive`, `front`) cover every dependency's
//! contents. Re-reading them on every build costs: one model's compile reads more than a
//! thousand headers (most of them the toolchain's). So, as ccache's direct mode does, a
//! dependency whose status (size, modification and change times, inode and device) is
//! the one recorded counts as unchanged, and only the others are read and hashed again.
//!
//! Invariant: a status is recorded only when it can be trusted, that is when both of the
//! file's times are older than the run that read it by more than [`TRUST_MARGIN`]. A
//! file written during the run, or just before it, may change again within the
//! filesystem's timestamp granularity without its status changing; such a file is
//! hashed on every build.

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
    fn of(path: &Path) -> Option<(Self, SystemTime)> {
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
        let newest = u64::try_from(newest).ok()?;
        Some((stat, SystemTime::UNIX_EPOCH + Duration::from_nanos(newest)))
    }

    /// Elsewhere every dependency is hashed on every build.
    #[cfg(not(unix))]
    fn of(_path: &Path) -> Option<(Self, SystemTime)> {
        None
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
    /// Hashes every file of `deps` now, for a run that started at `started`; `None` when
    /// one cannot be read.
    pub(crate) fn record(deps: &[PathBuf], started: SystemTime) -> Option<Self> {
        let trusted_before = started.checked_sub(TRUST_MARGIN)?;
        let entries = deps
            .iter()
            .map(|path| {
                // The status first: a change after it shows as a new status next time.
                let stat = Stat::of(path)
                    .filter(|(_, newest)| *newest < trusted_before)
                    .map(|(stat, _)| stat);
                Some(Entry {
                    path: path.clone(),
                    digest: digest(path)?,
                    stat,
                })
            })
            .collect::<Option<_>>()?;
        Some(Self { entries })
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

    /// The key in `domain` over `pre` and the files' current contents: the recorded
    /// digest for a file whose trusted status is unchanged, a new one for every other.
    /// `None` when a file can no longer be read (no entry can be valid).
    pub(crate) fn current_key(&self, domain: &str, pre: &str) -> Option<String> {
        let digests = self
            .entries
            .iter()
            .map(|e| {
                let same = e.stat.is_some() && Stat::of(&e.path).map(|(s, _)| s) == e.stat;
                if same {
                    Some(e.digest.clone())
                } else {
                    digest(&e.path)
                }
            })
            .collect::<Option<Vec<_>>>()?;
        Some(key(
            domain,
            pre,
            self.entries.iter().map(|e| &e.path).zip(digests),
        ))
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

    /// A run that started a minute from now: every file written before is trusted.
    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(60)
    }

    #[test]
    fn the_key_tracks_contents_and_a_vanished_file_is_a_miss() {
        let t = tempfile::tempdir().unwrap();
        let h = old_file(t.path(), "k.h", "#define K 1\n");
        let m = Manifest::record(std::slice::from_ref(&h), later()).unwrap();
        let k1 = m.recorded_key("d", "pre");
        assert_eq!(m.current_key("d", "pre").unwrap(), k1);
        assert_ne!(m.current_key("d", "other").unwrap(), k1);
        // Same size: only the status shows the change.
        std::fs::write(&h, "#define K 2\n").unwrap();
        assert_ne!(m.current_key("d", "pre").unwrap(), k1);
        std::fs::remove_file(&h).unwrap();
        assert_eq!(
            m.current_key("d", "pre"),
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
        let m = Manifest::record(std::slice::from_ref(&h), later()).unwrap();
        let text = m.text().replace(&m.entries[0].digest, "recorded");
        let stale = Manifest::parse(&text).unwrap();
        let expect = key("d", "pre", std::iter::once((&h, "recorded".to_owned())));
        assert_eq!(stale.current_key("d", "pre").unwrap(), expect);
        // Without a status (here: no longer trusted), the file is hashed.
        let untrusted =
            Manifest::parse(&text.replace(&Stat::of(&h).unwrap().0.text(), "-")).unwrap();
        assert_eq!(
            untrusted.current_key("d", "pre").unwrap(),
            m.recorded_key("d", "pre")
        );
    }

    /// A file written after the run started may change again with the same status, so
    /// its status is not recorded.
    #[test]
    fn a_status_newer_than_the_run_is_not_trusted() {
        let t = tempfile::tempdir().unwrap();
        let h = old_file(t.path(), "k.h", "x");
        let early = SystemTime::now() - Duration::from_secs(60);
        let m = Manifest::record(std::slice::from_ref(&h), early).unwrap();
        assert_eq!(m.entries[0].stat, None);
        assert!(
            Manifest::record(&[h], later()).unwrap().entries[0]
                .stat
                .is_some()
        );
    }

    #[test]
    fn manifests_round_trip_and_reject_other_text() {
        let t = tempfile::tempdir().unwrap();
        let deps = [
            old_file(t.path(), "a.h", "a"),
            old_file(t.path(), "b.h", "b"),
        ];
        let m = Manifest::record(&deps, later()).unwrap();
        assert_eq!(Manifest::parse(&m.text()), Some(m.clone()));
        assert_eq!(m.paths(), deps);
        assert_eq!(Manifest::parse("/a.h\n/b.h\n"), None, "an older plain list");
    }
}
