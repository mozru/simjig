//! The content-hash cache shared by every crate (and test crate) of a workspace.
//!
//! Layout under the cache root (see [`crate::BuildEnv::cache_dir`]):
//! - `models/<full key>/obj/`: Verilator output; `models/<full key>/deps`: every file
//!   Verilator read (its `.d` file's list).
//! - `manifests/<pre key>`: the input list of the last build with that pre key (of a
//!   verilation, of an archive compile, see `archive`, or of a DUT's IR, see `front`).
//! - `inputs/<hash>/<name>`: generated Verilator inputs, content-addressed.
//! - `runtime/<hash>/`: the embedded C ABI runtime sources, content-addressed.
//! - `prefixes/<scheme>/<prefix>`: the identity of the configuration that owns a model
//!   prefix, per identity scheme (see `pipeline::claim`).
//! - `libverilated/<key>/`: compiled Verilator runtimes (see `compile`).
//! - `archives/<key>/`: compiled models (see `compile`).
//! - `fronts/<key>`: the IR of a DUT; `checks/<key>`: a layout check that passed (see
//!   `front`).
//! - `tmp/`: work directories, renamed into place when complete; one left by a killed
//!   build is removed an hour later.
//!
//! Keys. Verilator only reveals which files it read (`` `include ``s) after running, so
//! the lookup has two steps, like ccache's direct mode:
//! 1. the *pre key* hashes what is known up front: [`crate::ABI_VERSION`], the
//!    `simjig-build` version, the full Verilator version line, every argument, the
//!    contents of every listed source and the file names in every include directory
//!    (so a new file that would shadow an include changes the key);
//! 2. the manifest for that pre key names the files the last such build read (but the
//!    Verilator binary, see [`is_verilator_binary`]); the *full key* adds their current
//!    contents (read again only for files whose status changed, see `manifest`). The
//!    entry for the full key is a hit.
//!
//! An input edited while Verilator runs (see `manifest`) leaves the output out of the
//! cache: it serves the build that made it, from `OUT_DIR` ([`keep_unpublished`]), and
//! the next build verilates again.
//!
//! Invariants: an entry directory is only ever created by an atomic `rename` of a
//! complete work directory, so readers never see a partial entry; entries are never
//! modified after that; concurrent builders of the same key both succeed (the loser
//! discards its copy).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::dut::{CacheStatus, GeneratedInput};
use crate::hash::KeyHasher;
use crate::manifest::{Digests, Manifest, Recorded, Snapshot, escape_path, unescape_path};
use crate::verilator::{self, Verilator};
use crate::{ABI_VERSION, Error, depfile};

/// The open cache root.
#[derive(Debug)]
pub(crate) struct Cache {
    root: PathBuf,
}

/// Verilator output, found in or added to the cache.
#[derive(Debug)]
pub(crate) struct Verilated {
    pub(crate) obj_dir: PathBuf,
    /// Every file Verilator read, from its `.d` file.
    pub(crate) deps: Vec<PathBuf>,
    pub(crate) status: CacheStatus,
    /// A dependency that changed while Verilator read it: this output serves this build
    /// only and is not in the cache (see `manifest`).
    pub(crate) changed: Option<PathBuf>,
}

impl Cache {
    pub(crate) fn open(root: PathBuf) -> Result<Self, Error> {
        for sub in [
            "models",
            "manifests",
            "inputs",
            "runtime",
            "prefixes",
            "libverilated",
            "archives",
            "fronts",
            "checks",
            "tmp",
        ] {
            let dir = root.join(sub);
            std::fs::create_dir_all(&dir).map_err(Error::io("creating", &dir))?;
        }
        remove_stale_work_dirs(&root.join("tmp"));
        Ok(Self { root })
    }

    /// Writes a generated input under a content-addressed path and returns that path.
    pub(crate) fn store_input(&self, input: &GeneratedInput) -> Result<PathBuf, Error> {
        let dir = self.store_files("inputs", &[(&input.file_name, &input.contents)])?;
        Ok(dir.join(&input.file_name))
    }

    /// Writes `files` (name, contents) into `<kind>/<hash of all of them>/` unless that
    /// directory exists, and returns it.
    pub(crate) fn store_files(&self, kind: &str, files: &[(&str, &str)]) -> Result<PathBuf, Error> {
        let mut h = KeyHasher::new("simjig-input-v1");
        for (name, contents) in files {
            h.str(name).str(contents);
        }
        let dir = self.root.join(kind).join(h.finish());
        if !dir.is_dir() {
            let work = self.work_dir()?;
            for (name, contents) in files {
                let tmp = work.join(name);
                std::fs::write(&tmp, contents).map_err(Error::io("writing", &tmp))?;
            }
            Self::publish(&work, &dir)?;
        }
        Ok(dir)
    }

    /// Returns Verilator's output for `args`, running Verilator only on a miss.
    /// `dut` names the DUT in a failure message; `out_dir` keeps an output that cannot be
    /// cached (see [`keep_unpublished`]).
    pub(crate) fn verilate(
        &self,
        dut: &str,
        verilator: &Verilator,
        args: &[String],
        include_dirs: &[PathBuf],
        out_dir: &Path,
    ) -> Result<Verilated, Error> {
        let pre = pre_key(verilator, args, include_dirs)?;
        let manifest = self.root.join("manifests").join(&pre);
        let prefix = prefix_of(args);
        let label = format!("verilated-{prefix}");
        let known = match self.lookup(&pre, &manifest) {
            Ok(hit) => {
                drop_unpublished(out_dir, &label);
                return Ok(hit);
            }
            Err(known) => known,
        };
        let work = self.work_dir()?;
        let snapshot = Snapshot::start(&work, known);
        let obj = work.join("obj");
        let mut cmd = verilator.command();
        cmd.arg("-Mdir").arg(&obj).args(args);
        if let Err(e) = verilator::run(&mut cmd, &format!("verilating DUT `{dut}`")) {
            remove_best_effort(&work);
            return Err(e);
        }
        let deps = read_deps(&obj, prefix)?;
        let recorded = match Manifest::record(&keyed_deps(&deps), &snapshot) {
            Recorded::Stable(m) => m,
            Recorded::Changed(path) => {
                let dir = keep_unpublished(&work, out_dir, &label)?;
                return Ok(Verilated {
                    obj_dir: dir.join("obj"),
                    deps,
                    status: CacheStatus::Miss,
                    changed: Some(path),
                });
            }
        };
        let deps_file = work.join("deps");
        std::fs::write(&deps_file, deps_listing(&deps))
            .map_err(Error::io("writing", &deps_file))?;
        let entry = self
            .root
            .join("models")
            .join(recorded.recorded_key(DEPS_DOMAIN, &pre));
        Self::publish(&work, &entry)?;
        self.write_atomic(&manifest, &recorded.text())?;
        drop_unpublished(out_dir, &label);
        Ok(Verilated {
            obj_dir: entry.join("obj"),
            deps,
            status: CacheStatus::Miss,
            changed: None,
        })
    }

    /// The entry for `pre` if the manifest's files still hash to one; else their current
    /// digests, if there is a manifest, for the run's snapshot.
    fn lookup(&self, pre: &str, manifest: &Path) -> Result<Verilated, Option<Digests>> {
        let current = Manifest::read(manifest)
            .and_then(|m| m.current())
            .ok_or(None)?;
        let entry = self.root.join("models").join(current.key(DEPS_DOMAIN, pre));
        // The entry's own list has the Verilator binary too, which the key leaves out.
        let listed = std::fs::read_to_string(entry.join("deps")).ok();
        let deps = listed.as_deref().and_then(parse_deps_listing);
        match deps {
            Some(deps) if entry.join("obj").is_dir() => Ok(Verilated {
                obj_dir: entry.join("obj"),
                deps,
                status: CacheStatus::Hit,
                changed: None,
            }),
            _ => Err(Some(current)),
        }
    }

    /// A fresh, empty work directory under `tmp/`.
    pub(crate) fn work_dir(&self) -> Result<PathBuf, Error> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = self
            .root
            .join("tmp")
            .join(format!("{}-{n}-{}", std::process::id(), nanos()));
        std::fs::create_dir_all(&dir).map_err(Error::io("creating", &dir))?;
        Ok(dir)
    }

    /// Moves a complete work directory to `entry`. If another builder got there first,
    /// its (identical) entry wins and ours is discarded.
    pub(crate) fn publish(work: &Path, entry: &Path) -> Result<(), Error> {
        if let Some(parent) = entry.parent() {
            std::fs::create_dir_all(parent).map_err(Error::io("creating", parent))?;
        }
        match std::fs::rename(work, entry) {
            Ok(()) => Ok(()),
            Err(_) if entry.is_dir() => {
                remove_best_effort(work);
                Ok(())
            }
            Err(e) => Err(Error::Io {
                op: "renaming a cache entry into",
                path: entry.to_path_buf(),
                source: e,
            }),
        }
    }

    /// Replaces the file at `path` with `contents` atomically.
    pub(crate) fn write_atomic(&self, path: &Path, contents: &str) -> Result<(), Error> {
        let work = self.work_dir()?;
        let tmp = work.join("file");
        std::fs::write(&tmp, contents).map_err(Error::io("writing", &tmp))?;
        // Replacing a file by rename is atomic, unlike replacing a directory.
        std::fs::rename(&tmp, path).map_err(Error::io("renaming into", path))?;
        remove_best_effort(&work);
        Ok(())
    }

    /// Records that `identity` owns `prefix` under identity `scheme` (see
    /// `pipeline::claim`), unless another identity already does: then
    /// returns that one. A race between two different identities may let both pass;
    /// a collision of the 32-bit prefix hash is already unlikely, and the loser's next
    /// build reports it.
    pub(crate) fn claim_prefix(
        &self,
        scheme: &str,
        prefix: &str,
        identity: &str,
    ) -> Result<Option<String>, Error> {
        let dir = self.root.join("prefixes").join(scheme);
        std::fs::create_dir_all(&dir).map_err(Error::io("creating", &dir))?;
        let path = dir.join(prefix);
        match std::fs::read_to_string(&path) {
            Ok(owner) if owner == identity => Ok(None),
            Ok(owner) => Ok(Some(owner)),
            Err(_) => {
                self.write_atomic(&path, identity)?;
                Ok(None)
            }
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `path` is in an output kept out of the cache (see [`keep_unpublished`]),
    /// where it stays or where it falls back to.
    pub(crate) fn is_unpublished(&self, path: &Path, out_dir: &Path) -> bool {
        path.starts_with(unpublished_dir(out_dir)) || path.starts_with(self.root.join("tmp"))
    }
}

fn pre_key(
    verilator: &Verilator,
    args: &[String],
    include_dirs: &[PathBuf],
) -> Result<String, Error> {
    let mut h = KeyHasher::new("simjig-verilate-v1");
    h.str(&ABI_VERSION.to_string())
        .str(env!("CARGO_PKG_VERSION"))
        .str(&verilator.version_line);
    for a in args {
        h.str(a);
    }
    // Sources are the arguments that name existing files; hash what is in them.
    for a in args {
        let p = Path::new(a);
        if p.is_absolute() && p.is_file() {
            h.file(p)?;
        }
    }
    for dir in include_dirs {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .map_err(Error::io("listing", dir))?
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();
        names.sort();
        h.path(dir);
        for n in names {
            h.field(n.as_encoded_bytes());
        }
    }
    Ok(h.finish())
}

/// The domain of the full keys (see the module docs).
const DEPS_DOMAIN: &str = "simjig-verilate-deps-v2";

/// The dependencies the full key covers: all but the Verilator binary.
fn keyed_deps(deps: &[PathBuf]) -> Vec<PathBuf> {
    deps.iter()
        .filter(|d| !is_verilator_binary(d))
        .cloned()
        .collect()
}

/// The Verilator executable shows up in its own `.d` file. Its version line is already in
/// the pre key, and hashing a multi-megabyte binary on every build would be wasted work.
pub(crate) fn is_verilator_binary(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("verilator_bin"))
}

fn read_deps(obj: &Path, prefix: &str) -> Result<Vec<PathBuf>, Error> {
    let path = verilator::output_file(obj, prefix, "__ver.d");
    let text = std::fs::read_to_string(&path).map_err(Error::io("reading", &path))?;
    depfile::parse(&path, &text)
}

fn prefix_of(args: &[String]) -> &str {
    args.iter()
        .position(|a| a == "--prefix")
        .and_then(|i| args.get(i + 1))
        .map_or("", String::as_str)
}

/// An entry's `deps` file: one escaped path per line (see [`escape_path`]), since a
/// file name may hold a line break.
fn deps_listing(deps: &[PathBuf]) -> String {
    let mut out = String::new();
    for d in deps {
        out.push_str(&escape_path(d));
        out.push('\n');
    }
    out
}

/// The paths [`deps_listing`] wrote; `None` when a line is not one it writes.
fn parse_deps_listing(text: &str) -> Option<Vec<PathBuf>> {
    text.lines().map(unescape_path).collect()
}

/// Moves `work`, the output of a run whose input changed while it ran, to
/// `OUT_DIR/simjig-unpublished/<label>` (replacing an earlier one) and returns where it
/// is. It serves this build only, and must outlive the build script: the link that
/// follows reads it. When it cannot be moved there (`OUT_DIR` on another filesystem than
/// the cache), it stays in `tmp/`, removed an hour later; by then Cargo has run the
/// build script again, since the edited input is newer than its last run.
pub(crate) fn keep_unpublished(work: &Path, out_dir: &Path, label: &str) -> Result<PathBuf, Error> {
    let parent = unpublished_dir(out_dir);
    std::fs::create_dir_all(&parent).map_err(Error::io("creating", &parent))?;
    let dst = parent.join(label);
    if dst.exists() {
        std::fs::remove_dir_all(&dst).map_err(Error::io("removing", &dst))?;
    }
    Ok(match std::fs::rename(work, &dst) {
        Ok(()) => dst,
        Err(_) => work.to_path_buf(),
    })
}

/// Where [`keep_unpublished`] keeps outputs.
fn unpublished_dir(out_dir: &Path) -> PathBuf {
    out_dir.join("simjig-unpublished")
}

/// Removes the output an earlier build kept for `label` (see [`keep_unpublished`]), once
/// a later build of that label made a good one: nothing links it any more. Only for a
/// label one DUT owns: a Verilator runtime's is shared by every DUT of the build script,
/// and an earlier DUT of this run may link it (it is replaced by the next one kept).
pub(crate) fn drop_unpublished(out_dir: &Path, label: &str) {
    let dir = unpublished_dir(out_dir).join(label);
    if dir.exists() {
        remove_best_effort(&dir);
    }
}

/// How old a work directory must be before it counts as left behind by a killed build.
/// No compile or verilation of one model takes an hour; a younger directory may belong to
/// a build that is still running (in another process sharing the cache).
const STALE_WORK_DIR: std::time::Duration = std::time::Duration::from_secs(3600);

/// Removes the work directories under `tmp` last modified more than [`STALE_WORK_DIR`] ago.
fn remove_stale_work_dirs(tmp: &Path) {
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let modified = e.metadata().and_then(|m| m.modified());
        let age = modified.ok().and_then(|m| now.duration_since(m).ok());
        if age.is_some_and(|a| a > STALE_WORK_DIR) {
            remove_best_effort(&e.path());
        }
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Leftover work directories only cost disk space; failing the build over one would be
/// worse than leaking it.
pub(crate) fn remove_best_effort(dir: &Path) {
    if std::fs::remove_dir_all(dir).is_err() {
        // Intentionally ignored: see above.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_is_first_writer_wins() {
        let t = tempfile::tempdir().unwrap();
        let cache = Cache::open(t.path().to_path_buf()).unwrap();
        let entry = t.path().join("models/k");
        for contents in ["first", "second"] {
            let work = cache.work_dir().unwrap();
            std::fs::write(work.join("f"), contents).unwrap();
            Cache::publish(&work, &entry).unwrap();
            assert!(!work.exists(), "work dir is consumed either way");
        }
        assert_eq!(std::fs::read_to_string(entry.join("f")).unwrap(), "first");
    }

    #[test]
    fn stored_inputs_are_content_addressed() {
        let t = tempfile::tempdir().unwrap();
        let cache = Cache::open(t.path().to_path_buf()).unwrap();
        let input = |c: &str| GeneratedInput {
            file_name: "p.vlt".into(),
            contents: c.into(),
        };
        let a = cache.store_input(&input("`verilator_config\n")).unwrap();
        assert_eq!(a, cache.store_input(&input("`verilator_config\n")).unwrap());
        let b = cache
            .store_input(&input("`verilator_config\n// x\n"))
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(a.file_name().unwrap(), "p.vlt");
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "`verilator_config\n// x\n"
        );
    }

    #[test]
    fn deps_listings_round_trip_paths_with_escapes_and_line_breaks() {
        let deps = [
            "/r/a\\b.svh",
            "/r/two\nlines.svh",
            "/r/ends-in-cr\r",
            "/r/plain.sv",
        ]
        .map(PathBuf::from);
        let text = deps_listing(&deps);
        assert_eq!(
            text.lines().count(),
            deps.len(),
            "one line per path: {text:?}"
        );
        assert_eq!(parse_deps_listing(&text).as_deref(), Some(&deps[..]));
    }

    #[test]
    fn the_verilator_binary_is_left_out_of_the_key() {
        let deps = [
            "/v/bin/verilator_bin",
            "/r/defs.svh",
            "/v/bin/verilator_bin_dbg",
        ]
        .map(PathBuf::from);
        assert_eq!(keyed_deps(&deps), [PathBuf::from("/r/defs.svh")]);
    }
}
