//! Compiled static libraries in the shared cache: the models (`archives/<key>/`) and the
//! Verilator runtimes (`libverilated/<key>/`), each holding one `lib<name>.a`.
//!
//! A C++ compile reads headers that no configuration names: the ones next to a source
//! (`#include "near.h"`), in an include directory, in a `-include`d file, in the
//! toolchain. So the key has two steps, like the Verilator cache (`cache.rs`) and ccache's
//! direct mode:
//! 1. the *pre key* hashes what is known up front: the kind, [`crate::ABI_VERSION`], the
//!    `simjig-build` version (which covers the fixed flags of `compile::base_build`), the
//!    Verilator version and include directory, the compiler (its path and arguments, and
//!    [`compiler_identity`]), the target, optimisation and debug settings, the defines,
//!    every source (path and contents), the include paths, and the file names in every
//!    directory a quoted include may be found in ([`search_dirs`]), so a new file that
//!    would shadow a header changes the key; directories simjig generates are not listed
//!    (`Toolchain::is_generated`);
//! 2. every compile runs with `-MD`, and the manifest `manifests/<pre key>` lists every
//!    file the compiler read (system headers included); the *full key* adds their current
//!    contents (read again only for files whose status changed, see `manifest`). The
//!    entry for the full key is a hit.
//!
//! A Verilator runtime is looked up once per build script ([`RUNTIMES`]): every model of
//! the script links the same one, and its sources do not change while the script runs.
//!
//! Invariants: an entry directory is published by an atomic rename of a complete work
//! directory holding only the archive (the objects are inside it); an entry directory
//! without its archive (from an interrupted older layout) is a miss, and is replaced.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::cache::{self, Cache};
use crate::compile::Inputs;
use crate::hash::KeyHasher;
use crate::manifest::Manifest;
use crate::{ABI_VERSION, BuildEnv, Error, depfile};

/// A static library, compiled or found in the cache.
#[derive(Debug, Clone)]
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
    /// [`compiler_identity`] of `compiler`.
    pub(crate) identity: String,
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

/// The domain of the full keys (see the module docs).
const DEPS_DOMAIN: &str = "simjig-archive-deps-v2";

/// The Verilator runtimes this process found, by (`OUT_DIR`, cache root, pre key). Keyed
/// by `OUT_DIR` like `script`, so tests that stand for several build scripts in one
/// process look again.
static RUNTIMES: Mutex<Vec<(RuntimeKey, Archive)>> = Mutex::new(Vec::new());

/// (`OUT_DIR`, cache root, pre key).
type RuntimeKey = (PathBuf, PathBuf, String);

impl Toolchain<'_> {
    /// Returns the archive of `spec` from the cache, compiling it on a miss.
    pub(crate) fn cached_archive(&self, spec: &Spec<'_>) -> Result<Archive, Error> {
        let pre = self.pre_key(spec)?;
        if spec.kind != "libverilated" {
            return self.cached_archive_keyed(spec, &pre);
        }
        let memo = (
            self.i.env.out_dir.clone(),
            self.i.cache.root().to_path_buf(),
            pre,
        );
        // A panic while holding the lock leaves the list itself consistent.
        let known = RUNTIMES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(k, _)| *k == memo)
            .map(|(_, a)| a.clone());
        if let Some(a) = known {
            return Ok(a);
        }
        let a = self.cached_archive_keyed(spec, &memo.2)?;
        RUNTIMES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((memo, a.clone()));
        Ok(a)
    }

    fn cached_archive_keyed(&self, spec: &Spec<'_>, pre: &str) -> Result<Archive, Error> {
        let cache = self.i.cache;
        let manifest = cache.root().join("manifests").join(pre);
        if let Some(hit) = lookup(cache, spec, pre, &manifest) {
            return Ok(hit);
        }
        let started = std::time::SystemTime::now();
        let work = cache.work_dir()?;
        let compiled = self.compile_in(&work, spec);
        let (lib, deps) = match compiled {
            Ok(c) => c,
            Err(e) => {
                cache::remove_best_effort(&work);
                return Err(e);
            }
        };
        let recorded = Manifest::record(&deps, started).ok_or_else(|| Error::Parse {
            path: work.clone(),
            line: 0,
            msg: "a file the compiler read vanished before it could be hashed".to_owned(),
        })?;
        let full = recorded.recorded_key(DEPS_DOMAIN, pre);
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
        cache.write_atomic(&manifest, &recorded.text())?;
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
            .str(&self.identity)
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
            if !self.is_generated(&d) {
                hash_listing(&mut h, &d);
            }
        }
        Ok(h.finish())
    }

    /// Whether simjig generates the files in `dir`: the cache, whose entries never change
    /// once published, and `OUT_DIR/simjig/`, whose files are named after the DUT and the
    /// prefix, which the key has already. Nothing else writes there, so no new file in
    /// them can shadow a header; and listing `OUT_DIR/simjig/`, which holds every DUT's
    /// files, made every model's key change with every other DUT.
    fn is_generated(&self, dir: &Path) -> bool {
        dir.starts_with(self.i.cache.root())
            || dir.starts_with(crate::emit::generated_dir(&self.i.env.out_dir))
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

/// What identifies the C++ compiler beyond its path: the executable the path resolves to
/// (through `PATH` and symlinks) with its size and modification time, and the output of
/// `<compiler> --version`. An upgrade in place keeps the path but changes these, and with
/// them the system headers and code generation.
///
/// `--version` runs once per build script (`OUT_DIR`) and executable: a build script
/// compiles several archives per DUT, and the answer does not change while it runs.
pub(crate) fn compiler_identity(env: &BuildEnv, tool: &cc::Tool) -> String {
    static VERSIONS: Mutex<Vec<(PathBuf, PathBuf, String)>> = Mutex::new(Vec::new());
    let exe = resolve(env, tool.path()).1;
    let file = match std::fs::metadata(&exe) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{} {} {mtime}", exe.display(), m.len())
        }
        Err(e) => format!("{} unreadable: {e}", exe.display()),
    };
    // A panic while holding the lock leaves the list itself consistent.
    let mut versions = VERSIONS.lock().unwrap_or_else(PoisonError::into_inner);
    let known = versions
        .iter()
        .find(|(out, path, _)| *out == env.out_dir && *path == exe)
        .map(|(_, _, v)| v.clone());
    let version = known.unwrap_or_else(|| {
        let v = version_output(&exe);
        versions.push((env.out_dir.clone(), exe.clone(), v.clone()));
        v
    });
    format!("{file}\n{version}")
}

/// The files whose change must rerun the build script because they are the compiler: the
/// path `tool` names (found on `PATH` when it is a bare name: a symlink or a wrapper
/// script), the executable that resolves to, and the same for a wrapper `cc` runs it
/// through (`CXX="ccache c++"`). An upgrade in place changes them while `CXX` and `PATH`
/// stay the same, and neither variable would rerun the script.
pub(crate) fn compiler_files(env: &BuildEnv, tool: &cc::Tool) -> Vec<PathBuf> {
    // The program of the command `cc` runs: the wrapper, if there is one.
    let program = PathBuf::from(tool.to_command().get_program());
    let mut files = Vec::new();
    for named in [tool.path(), program.as_path()] {
        let (found, exe) = resolve(env, named);
        files.extend([found, exe]);
    }
    files.sort();
    files.dedup();
    files
}

/// The path `named` is found at (through `PATH` when it is a bare name), and the
/// executable that path resolves to through symlinks.
fn resolve(env: &BuildEnv, named: &Path) -> (PathBuf, PathBuf) {
    let found = if named.components().count() == 1 {
        env.find_on_path(&named.to_string_lossy())
    } else {
        None
    };
    let found = found.unwrap_or_else(|| named.to_path_buf());
    let exe = std::fs::canonicalize(&found).unwrap_or_else(|_| found.clone());
    (found, exe)
}

/// `<exe> --version`, with its status and both streams. A compiler that cannot answer
/// (MSVC has no `--version`) is identified by the failure instead.
fn version_output(exe: &Path) -> String {
    match std::process::Command::new(exe).arg("--version").output() {
        Ok(o) => format!(
            "{}\n{}{}",
            o.status,
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => format!("could not run: {e}"),
    }
}

/// The cached archive for `pre`, if the manifest's dependencies still hash to an entry.
fn lookup(cache: &Cache, spec: &Spec<'_>, pre: &str, manifest: &Path) -> Option<Archive> {
    let m = Manifest::read(manifest)?;
    let full = m.current_key(DEPS_DOMAIN, pre)?;
    let name = archive_name(spec.what, &full);
    let dir = cache.root().join(spec.kind).join(&full);
    dir.join(format!("lib{name}.a")).is_file().then(|| Archive {
        dir,
        name,
        deps: m.paths(),
    })
}

fn archive_name(what: &str, full: &str) -> String {
    format!("simjig_{what}_{}", &full[..16])
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
}
