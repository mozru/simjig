//! The content-hash cache shared by every crate (and test crate) of a workspace.
//!
//! Layout under the cache root (see [`crate::BuildEnv::cache_dir`]):
//! - `models/<full key>/obj/`: Verilator output; `models/<full key>/deps`: its inputs.
//! - `manifests/<pre key>`: the input list of the last build with that pre key.
//! - `inputs/<hash>/<name>`: generated Verilator inputs, content-addressed.
//! - `runtime/<hash>/`: the embedded C ABI runtime sources, content-addressed.
//! - `prefixes/<prefix>`: the identity of the configuration that owns a model prefix.
//! - `libverilated/<key>/`: compiled Verilator runtimes (see `compile`).
//! - `tmp/`: work directories, renamed into place when complete.
//!
//! Keys. Verilator only reveals which files it read (`` `include ``s) after running, so
//! the lookup has two steps, like ccache's direct mode:
//! 1. the *pre key* hashes what is known up front: [`crate::ABI_VERSION`], the
//!    `simjig-build` version, the full Verilator version line, every argument, the
//!    contents of every listed source and the file names in every include directory
//!    (so a new file that would shadow an include changes the key);
//! 2. the manifest for that pre key names the files the last such build read; the *full
//!    key* adds their current contents. The entry for the full key is a hit.
//!
//! Invariants: an entry directory is only ever created by an atomic `rename` of a
//! complete work directory, so readers never see a partial entry; entries are never
//! modified after that; concurrent builders of the same key both succeed (the loser
//! discards its copy).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::dut::{CacheStatus, GeneratedInput};
use crate::hash::KeyHasher;
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
            "tmp",
        ] {
            let dir = root.join(sub);
            std::fs::create_dir_all(&dir).map_err(Error::io("creating", &dir))?;
        }
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
    pub(crate) fn verilate(
        &self,
        verilator: &Verilator,
        args: &[String],
        include_dirs: &[PathBuf],
    ) -> Result<Verilated, Error> {
        let pre = pre_key(verilator, args, include_dirs)?;
        let manifest = self.root.join("manifests").join(&pre);
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            let deps: Vec<PathBuf> = text.lines().map(PathBuf::from).collect();
            if let Some(full) = full_key(&pre, &deps) {
                let entry = self.root.join("models").join(full);
                if entry.join("obj").is_dir() {
                    return Ok(Verilated {
                        obj_dir: entry.join("obj"),
                        deps,
                        status: CacheStatus::Hit,
                    });
                }
            }
        }
        let work = self.work_dir()?;
        let obj = work.join("obj");
        let mut cmd = verilator.command();
        cmd.arg("-Mdir").arg(&obj).args(args);
        if let Err(e) = verilator::run(&mut cmd) {
            remove_best_effort(&work);
            return Err(e);
        }
        let deps = read_deps(&obj, prefix_of(args))?;
        let full = full_key(&pre, &deps).ok_or_else(|| Error::Parse {
            path: obj.clone(),
            line: 0,
            msg: "a file Verilator read vanished before it could be hashed".to_owned(),
        })?;
        let listing = deps_listing(&deps);
        let deps_file = work.join("deps");
        std::fs::write(&deps_file, &listing).map_err(Error::io("writing", &deps_file))?;
        let entry = self.root.join("models").join(full);
        Self::publish(&work, &entry)?;
        self.write_atomic(&manifest, &listing)?;
        Ok(Verilated {
            obj_dir: entry.join("obj"),
            deps,
            status: CacheStatus::Miss,
        })
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

    fn write_atomic(&self, path: &Path, contents: &str) -> Result<(), Error> {
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

/// `None` when a dependency can no longer be read (the entry cannot be valid).
fn full_key(pre: &str, deps: &[PathBuf]) -> Option<String> {
    let mut h = KeyHasher::new("simjig-verilate-deps-v1");
    h.str(pre);
    for d in deps.iter().filter(|d| !is_verilator_binary(d)) {
        h.file(d).ok()?;
    }
    Some(h.finish())
}

/// The Verilator executable shows up in its own `.d` file. Its version line is already in
/// the pre key, and hashing a multi-megabyte binary on every build would be wasted work.
fn is_verilator_binary(p: &Path) -> bool {
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

fn deps_listing(deps: &[PathBuf]) -> String {
    let mut out = String::new();
    for d in deps {
        out.push_str(&d.display().to_string());
        out.push('\n');
    }
    out
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
    fn full_key_tracks_dependency_contents() {
        let t = tempfile::tempdir().unwrap();
        let inc = t.path().join("defs.svh");
        let bin = t.path().join("verilator_bin");
        std::fs::write(&inc, "`define STEP 1\n").unwrap();
        std::fs::write(&bin, "binary v1").unwrap();
        let deps = [bin.clone(), inc.clone()];
        let k1 = full_key("pre", &deps).unwrap();
        std::fs::write(&bin, "binary v2").unwrap();
        assert_eq!(
            full_key("pre", &deps).unwrap(),
            k1,
            "the binary is covered by the version"
        );
        std::fs::write(&inc, "`define STEP 2\n").unwrap();
        assert_ne!(
            full_key("pre", &deps).unwrap(),
            k1,
            "an included file changed"
        );
        assert_ne!(
            full_key("other", &deps).unwrap(),
            full_key("pre", &deps).unwrap()
        );
        std::fs::remove_file(&inc).unwrap();
        assert_eq!(
            full_key("pre", &deps),
            None,
            "a vanished dependency is a miss"
        );
    }
}
