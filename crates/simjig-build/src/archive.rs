//! Compiled static libraries in the shared cache: the models (`archives/<key>/`) and the
//! Verilator runtimes (`libverilated/<key>/`), each holding one `lib<name>.a`.
//!
//! A C++ compile reads headers that no configuration names: the ones next to a source
//! (`#include "near.h"`), in an include directory, in a `-include`d file, in the
//! toolchain. So the key has two steps, like the Verilator cache (`cache.rs`) and ccache's
//! direct mode:
//! 1. the *pre key* hashes what is known up front: the kind, [`crate::ABI_VERSION`], the
//!    `simjig-build` version (which covers the fixed flags of `compile::base_build`), the
//!    Verilator version and include directory, the compiler's path and arguments,
//!    the target, optimisation and debug settings, the defines,
//!    every source (path and contents), the include paths, and the file names in every
//!    directory a quoted include may be found in ([`search_dirs`]), so a new file that
//!    would shadow a header changes the key;
//! 2. every compile runs with `-MD`, and the manifest `manifests/<pre key>` lists every
//!    file the compiler read (system headers included); the *full key* adds their current
//!    contents. The entry for the full key is a hit.
//!
//! Invariants: an entry directory is published by an atomic rename of a complete work
//! directory holding only the archive (the objects are inside it); an entry directory
//! without its archive (from an interrupted older layout) is a miss, and is replaced.

use std::path::{Path, PathBuf};

use crate::cache::{self, Cache};
use crate::compile::Inputs;
use crate::hash::KeyHasher;
use crate::{ABI_VERSION, Error, depfile};

/// A static library, compiled or found in the cache.
pub(crate) struct Archive {
    pub(crate) dir: PathBuf,
    pub(crate) name: String,
    /// Every file the compiler read for it.
    pub(crate) deps: Vec<PathBuf>,
}

/// What every archive of one build shares: the inputs, the configured base build and the
/// real compiler (before any ccache wrapping) with its identity.
pub(crate) struct Toolchain<'a> {
    pub(crate) i: &'a Inputs<'a>,
    pub(crate) base: &'a cc::Build,
    pub(crate) compiler: &'a cc::Tool,
}

/// One archive to build: `simjig_<what>_<key>` from `sources` into `<cache>/<kind>/`.
pub(crate) struct Spec<'a> {
    pub(crate) kind: &'a str,
    pub(crate) what: &'a str,
    pub(crate) sources: &'a [PathBuf],
    pub(crate) defines: &'a [(String, Option<String>)],
    /// Include paths for this archive only, beyond the base build's.
    pub(crate) includes: &'a [PathBuf],
}

impl Toolchain<'_> {
    /// Returns the archive of `spec` from the cache, compiling it on a miss.
    pub(crate) fn cached_archive(&self, spec: &Spec<'_>) -> Result<Archive, Error> {
        let cache = self.i.cache;
        let pre = self.pre_key(spec)?;
        let manifest = cache.root().join("manifests").join(&pre);
        if let Some(hit) = lookup(cache, spec, &pre, &manifest) {
            return Ok(hit);
        }
        let work = cache.work_dir()?;
        let compiled = self.compile_in(&work, spec);
        let (lib, deps) = match compiled {
            Ok(c) => c,
            Err(e) => {
                cache::remove_best_effort(&work);
                return Err(e);
            }
        };
        let full = full_key(&pre, &deps).ok_or_else(|| Error::Parse {
            path: work.clone(),
            line: 0,
            msg: "a file the compiler read vanished before it could be hashed".to_owned(),
        })?;
        let name = archive_name(spec.what, &full);
        // Publish the archive alone: its objects are inside it.
        let out = cache.work_dir()?;
        let dst = out.join(format!("lib{name}.a"));
        std::fs::rename(&lib, &dst).map_err(Error::io("renaming into", &dst))?;
        cache::remove_best_effort(&work);
        let entry = cache.root().join(spec.kind).join(&full);
        if entry.is_dir() && !entry.join(format!("lib{name}.a")).is_file() {
            cache::remove_best_effort(&entry);
        }
        Cache::publish(&out, &entry)?;
        cache.write_atomic(&manifest, &listing(&deps))?;
        Ok(Archive {
            dir: entry,
            name,
            deps,
        })
    }

    fn pre_key(&self, spec: &Spec<'_>) -> Result<String, Error> {
        let i = self.i;
        let mut h = KeyHasher::new("simjig-archive-v4");
        h.str(spec.kind)
            .str(&ABI_VERSION.to_string())
            .str(env!("CARGO_PKG_VERSION"))
            .str(&i.verilator.version_line)
            .path(&i.verilator.include_dir)
            .path(self.compiler.path())
            .str(&i.env.target)
            .str(&i.env.cxx_opt)
            .field(&[u8::from(i.env.debug)])
            .str(spec.what);
        for f in self.compiler.args() {
            h.field(f.as_encoded_bytes());
        }
        for (k, v) in spec.defines {
            h.str(k).str(v.as_deref().unwrap_or("\0none"));
        }
        for s in spec.sources {
            h.file(s)?;
        }
        for d in spec.includes {
            h.path(d);
        }
        for d in search_dirs(i, spec) {
            hash_listing(&mut h, &d);
        }
        Ok(h.finish())
    }

    /// Compiles `spec` in `work` with `-MD`; returns the archive and every file read.
    fn compile_in(&self, work: &Path, spec: &Spec<'_>) -> Result<(PathBuf, Vec<PathBuf>), Error> {
        let what = spec.what;
        let mut b = self.base.clone();
        b.out_dir(work)
            .includes(spec.includes)
            .files(spec.sources)
            .flag("-MD");
        for (k, v) in spec.defines {
            b.define(k, v.as_deref());
        }
        let provisional = format!("simjig_{what}");
        if let Err(source) = b.try_compile(&provisional) {
            let what = match what.strip_prefix("model_") {
                Some(prefix) => format!("the `{prefix}` model"),
                None => format!("the Verilator runtime (`{what}`)"),
            };
            return Err(Error::Compile { what, source });
        }
        let deps = compiler_deps(work)?;
        Ok((work.join(format!("lib{provisional}.a")), deps))
    }
}

/// The cached archive for `pre`, if the manifest's dependencies still hash to an entry.
fn lookup(cache: &Cache, spec: &Spec<'_>, pre: &str, manifest: &Path) -> Option<Archive> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let deps: Vec<PathBuf> = text.lines().map(PathBuf::from).collect();
    let full = full_key(pre, &deps)?;
    let name = archive_name(spec.what, &full);
    let dir = cache.root().join(spec.kind).join(&full);
    dir.join(format!("lib{name}.a"))
        .is_file()
        .then_some(Archive { dir, name, deps })
}

fn archive_name(what: &str, full: &str) -> String {
    format!("simjig_{what}_{}", &full[..16])
}

/// `None` when a dependency can no longer be read (the entry cannot be valid).
fn full_key(pre: &str, deps: &[PathBuf]) -> Option<String> {
    let mut h = KeyHasher::new("simjig-archive-deps-v1");
    h.str(pre);
    for d in deps {
        h.file(d).ok()?;
    }
    Some(h.finish())
}

fn listing(deps: &[PathBuf]) -> String {
    let mut out = String::new();
    for d in deps {
        out.push_str(&d.display().to_string());
        out.push('\n');
    }
    out
}

/// Every prerequisite in the `.d` files under `work`, deduplicated, sorted. Relative
/// paths are relative to the compiler's working directory, this process's.
fn compiler_deps(work: &Path) -> Result<Vec<PathBuf>, Error> {
    let cwd = std::env::current_dir().map_err(Error::io("reading the current directory", work))?;
    let mut deps = Vec::new();
    let mut stack = vec![work.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(Error::io("listing", &d))? {
            let p = e.map_err(Error::io("listing", &d))?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "d") {
                let text = std::fs::read_to_string(&p).map_err(Error::io("reading", &p))?;
                deps.extend(depfile::parse(&p, &text)?.into_iter().map(|f| cwd.join(f)));
            }
        }
    }
    deps.sort();
    deps.dedup();
    Ok(deps)
}

/// The directories a quoted `#include` of `spec` may be resolved in, besides the fixed
/// runtime and Verilator directories: each source's directory, the archive's and the
/// recipe's include directories, and those named by `-I`, `-iquote`, `-isystem` and
/// `-idirafter` in the recipe flags.
fn search_dirs(i: &Inputs<'_>, spec: &Spec<'_>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = spec
        .sources
        .iter()
        .filter_map(|s| s.parent().map(Path::to_path_buf))
        .collect();
    dirs.extend(spec.includes.iter().cloned());
    dirs.extend(i.recipe.include_dirs.iter().cloned());
    dirs.extend(flag_dirs(&i.recipe.flags));
    dirs.sort();
    dirs.dedup();
    dirs
}

/// The directories named by include-path flags, joined (`-Ifoo`) or separate (`-I foo`).
fn flag_dirs(flags: &[String]) -> Vec<PathBuf> {
    const OPTS: &[&str] = &["-iquote", "-isystem", "-idirafter", "-I"];
    let mut out = Vec::new();
    let mut it = flags.iter();
    while let Some(f) = it.next() {
        let Some(opt) = OPTS.iter().find(|o| f.starts_with(**o)) else {
            continue;
        };
        let dir = match &f[opt.len()..] {
            "" => it.next().map(String::as_str),
            rest => Some(rest),
        };
        out.extend(dir.map(|d| std::path::absolute(d).unwrap_or_else(|_| PathBuf::from(d))));
    }
    out
}

/// Hashes the sorted file names directly in `dir` (none when it cannot be listed).
fn hash_listing(h: &mut KeyHasher, dir: &Path) {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    names.sort();
    h.path(dir);
    for n in names {
        h.field(n.as_encoded_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_flags_name_their_directories() {
        let flags: Vec<String> = ["-I/a", "-I", "/b", "-isystem/c", "-iquote", "/d", "-O2"]
            .map(str::to_owned)
            .into();
        assert_eq!(
            flag_dirs(&flags),
            ["/a", "/b", "/c", "/d"].map(PathBuf::from)
        );
    }

    #[test]
    fn full_key_tracks_dependency_contents() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("k.h");
        std::fs::write(&h, "#define K 1\n").unwrap();
        let deps = [h.clone()];
        let k1 = full_key("pre", &deps).unwrap();
        assert_eq!(full_key("pre", &deps).unwrap(), k1);
        std::fs::write(&h, "#define K 2\n").unwrap();
        assert_ne!(full_key("pre", &deps).unwrap(), k1);
        std::fs::remove_file(&h).unwrap();
        assert_eq!(full_key("pre", &deps), None, "a vanished header is a miss");
    }
}
