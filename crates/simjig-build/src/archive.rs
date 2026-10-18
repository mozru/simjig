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
//!    would shadow a header changes the key (a source's directory in the cache holds
//!    that source alone, and its listing never changes);
//! 2. every compile runs with `-MD`, and the manifest `manifests/<pre key>` lists every
//!    file the compiler read (system headers included); the *full key* adds their current
//!    contents (read again only for files whose status changed, see `manifest`). The
//!    entry for the full key is a hit.
//!
//! The model's sources are Verilator's output, in the verilation's entry
//! (`models/<full key>/obj/`), whose path is in the pre key. So any edit to a file
//! Verilator read compiles the model again, even one that leaves the output the same (a
//! comment in an included `.svh`). Accepted: keying on the output's contents instead would
//! hash every generated source on every build, and ccache (when present) makes such a
//! compile cheap. The shim is content-addressed (`emit`), so crates and target
//! directories that build the same configuration share one model archive.
//!
//! An input edited while the compiler reads it leaves the archive out of the cache (see
//! `manifest`): it is linked from `OUT_DIR/simjig-unpublished/` by that build alone. So is
//! a model archive compiled from a verilation kept out of the cache that way: its
//! sources' path is in its pre key, and names no entry a later build finds.
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
use crate::manifest::{Digests, Manifest, Recorded, Snapshot};
use crate::{ABI_VERSION, BuildEnv, Error, depfile};

/// A static library, compiled or found in the cache.
#[derive(Debug, Clone)]
pub(crate) struct Archive {
    pub(crate) dir: PathBuf,
    pub(crate) name: String,
    /// Every file the compiler read for it.
    pub(crate) deps: Vec<PathBuf>,
    /// A dependency that changed while the compiler read it: this archive serves this
    /// build only and is not in the cache (see `manifest`).
    pub(crate) changed: Option<PathBuf>,
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
    /// Compiler flags for this archive only, after the base build's.
    pub(crate) flags: &'a [String],
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
        if a.changed.is_some() {
            // The next DUT of this script compiles it again, from the edited input.
            return Ok(a);
        }
        RUNTIMES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((memo, a.clone()));
        Ok(a)
    }

    fn cached_archive_keyed(&self, spec: &Spec<'_>, pre: &str) -> Result<Archive, Error> {
        let cache = self.i.cache;
        let out_dir = &self.i.env.out_dir;
        let manifest = cache.root().join("manifests").join(pre);
        let label = format!("{}-{}", spec.kind, spec.what);
        // Per DUT (see `cache::drop_unpublished`).
        let owned = spec.kind == "archives";
        // Compiled from an output kept out of the cache (a model verilated while its input
        // changed): its path names no entry a later build finds, so neither is this.
        let transient = spec
            .sources
            .iter()
            .any(|s| cache.is_unpublished(s, out_dir));
        let found = if transient {
            Err(None)
        } else {
            lookup(cache, spec, pre, &manifest)
        };
        let known = match found {
            Ok(hit) => {
                if owned {
                    cache::drop_unpublished(out_dir, &label);
                }
                return Ok(hit);
            }
            Err(known) => known,
        };
        let work = cache.work_dir()?;
        let snapshot = Snapshot::start(&work, known);
        let compiled = self.compile_in(&work, spec);
        let (lib, deps) = match compiled {
            Ok(c) => c,
            Err(e) => {
                cache::remove_best_effort(&work);
                return Err(e);
            }
        };
        let recorded = match Manifest::record(&deps, &snapshot) {
            Recorded::Stable(m) if !transient => m,
            unpublished => {
                let dir = cache::keep_unpublished(&work, out_dir, &label)?;
                let changed = match unpublished {
                    Recorded::Changed(path) => Some(path),
                    Recorded::Stable(_) => None,
                };
                return Ok(Archive {
                    dir,
                    name: provisional_name(spec.what),
                    deps,
                    changed,
                });
            }
        };
        let full = recorded.recorded_key(DEPS_DOMAIN, pre);
        let (dir, name) = publish(cache, spec, &full, &lib, &work)?;
        cache.write_atomic(&manifest, &recorded.text())?;
        if owned {
            cache::drop_unpublished(out_dir, &label);
        }
        Ok(Archive {
            dir,
            name,
            deps,
            changed: None,
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
        for f in spec.flags {
            h.str(f);
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
            .flags(spec.flags)
            .files(spec.sources)
            .flag("-MD");
        for (k, v) in spec.defines {
            b.define(k, v.as_deref());
        }
        let provisional = provisional_name(what);
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

/// The directories of the toolchain's own headers, as the compiler (asked with its
/// arguments but without their include-path flags) and the SDK report them
/// ([`toolchain_from`]): the compiler's resource directory, its C++ library, the SDK.
/// Each as named and canonical.
///
/// A file under them changes only with the toolchain, which [`compiler_identity`] and
/// [`compiler_files`] already track, so the build script need not rerun on each (they
/// are most of a model's dependencies); the archive keys still hash them. Other
/// directories of the search list are not the compiler's own (a conda or Homebrew
/// prefix's `include/`, `/usr/local/include`, `/usr/include` on Linux): a library header
/// there may be upgraded on its own, and must rerun the script. Asked once per build
/// script (`OUT_DIR`) and compiler; a compiler that cannot answer gives what it could.
pub(crate) fn toolchain_dirs(env: &BuildEnv, tool: &cc::Tool) -> Vec<PathBuf> {
    static DIRS: Mutex<Vec<(PathBuf, PathBuf, Vec<PathBuf>)>> = Mutex::new(Vec::new());
    let (found, exe) = resolve(env, tool.path());
    // A panic while holding the lock leaves the list itself consistent.
    let mut known = DIRS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, _, dirs)) = known
        .iter()
        .find(|(out, path, _)| *out == env.out_dir && *path == exe)
    {
        return dirs.clone();
    }
    let args: Vec<&std::ffi::OsStr> = without_include_flags(tool.args());
    let ask = |program: &Path, args: &[&std::ffi::OsStr], extra: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .args(extra)
            .stdin(std::process::Stdio::null())
            .output()
            .map(|o| {
                let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
                (text(&o.stdout), text(&o.stderr))
            })
            .unwrap_or_default()
    };
    let report = ask(&found, &args, &["-E", "-x", "c++", "-v", "-"]).1;
    // Clang answers both; GCC only the second, with its own `include` directory.
    let resource_dir = ask(&found, &args, &["-print-resource-dir"]).0;
    let include_dir = ask(&found, &args, &["-print-file-name=include"]).0;
    let sdk =
        cfg!(target_os = "macos").then(|| ask(Path::new("xcrun"), &[], &["--show-sdk-path"]).0);
    let mut dirs = toolchain_from(&report, &resource_dir, &include_dir, sdk.as_deref());
    let canonical: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .collect();
    dirs.extend(canonical);
    let mut dirs: Vec<PathBuf> = dirs.iter().map(|d| lexical(d)).collect();
    dirs.sort();
    dirs.dedup();
    known.push((env.out_dir.clone(), exe, dirs.clone()));
    dirs
}

/// The toolchain's header directories in the compiler's answers: `report`, the stderr of
/// `-E -v`; `resource_dir` and `include_dir`, the stdout of `-print-resource-dir` and
/// `-print-file-name=include`; and `sdk`, that of `xcrun --show-sdk-path` on macOS. They
/// are
/// - the resource directory (Clang's `lib/clang/<ver>`, GCC's `lib/gcc/<triple>/<ver>`):
///   `resource_dir`, and the parent of `include_dir` when the search list has that
///   `include` (GCC's `include` and `include-fixed` are both under it), so that an answer
///   outside the search list cannot make a whole prefix the toolchain's;
/// - the C++ standard library's headers: the search list's directories with a `c++`
///   component (`include/c++/v1`, `include/c++/13`, `<triple>/c++/13`) under the
///   compiler's prefix (the directory above the resource directory's `lib/clang` or
///   `lib/gcc`, or above Clang's `InstalledDir`) or under the SDK, so a user's
///   `CPLUS_INCLUDE_PATH=$HOME/libs/c++` is not one;
/// - the SDK: `sdk`, and the `-isysroot` the report's compiler command line passes.
///
/// A relative answer (GCC prints `include` when it has none) is not a directory.
fn toolchain_from(
    report: &str,
    resource_dir: &str,
    include_dir: &str,
    sdk: Option<&str>,
) -> Vec<PathBuf> {
    let search = system_search_list(report);
    let absolute = |p: &str| Some(PathBuf::from(p.trim())).filter(|p| p.is_absolute());
    let included = absolute(include_dir)
        .filter(|d| d.ends_with("include") && search.iter().any(|s| lexical(s) == lexical(d)));
    let mut resources: Vec<PathBuf> = absolute(resource_dir).into_iter().collect();
    resources.extend(included.and_then(|d| d.parent().map(Path::to_path_buf)));
    let unquote = |t: &str| t.trim_matches(|c| c == '"' || c == '\'').to_owned();
    let words: Vec<&str> = report.split_whitespace().collect();
    let sysroots = words
        .windows(2)
        .filter(|w| unquote(w[0]) == "-isysroot")
        .map(|w| unquote(w[1]));
    let sdks: Vec<PathBuf> = sysroots
        .chain(sdk.map(|s| s.trim().to_owned()))
        .filter_map(|s| absolute(&s))
        .collect();
    let mut owned: Vec<PathBuf> = resources.iter().filter_map(|r| prefix_of(r)).collect();
    owned.extend(
        report
            .lines()
            .filter_map(|l| l.strip_prefix("InstalledDir: "))
            .filter_map(|d| absolute(d)?.parent().map(lexical)),
    );
    owned.extend(sdks.iter().map(|s| lexical(s)));
    let mut dirs: Vec<PathBuf> = search
        .into_iter()
        .filter(|d| {
            let mut parts = d.components().map(std::path::Component::as_os_str);
            parts.any(|c| c == "c++") && parts.next().is_some() && under_any(d, &owned)
        })
        .collect();
    dirs.extend(resources);
    dirs.extend(sdks);
    dirs.sort();
    dirs.dedup();
    dirs
}

/// The installation prefix above a resource directory: what precedes its `lib/clang` or
/// `lib/gcc` (`/usr` for `/usr/lib/gcc/x86_64-linux-gnu/13`), compared lexically.
fn prefix_of(resource: &Path) -> Option<PathBuf> {
    let resource = lexical(resource);
    let parts: Vec<_> = resource.components().collect();
    let at = parts.windows(2).rposition(|w| {
        w[0].as_os_str() == "lib" && (w[1].as_os_str() == "clang" || w[1].as_os_str() == "gcc")
    })?;
    Some(parts[..at].iter().collect())
}

/// Whether `path` is under one of `dirs` ([`toolchain_dirs`]), compared with `.` and
/// `..` removed as the compiler's `-MD` output may keep them (`bin/../include`).
pub(crate) fn under_any(path: &Path, dirs: &[PathBuf]) -> bool {
    let path = lexical(path);
    dirs.iter().any(|d| path.starts_with(d))
}

/// `path` with `.` dropped and each `..` taking off the component before it.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// `args` without the include-path flags (`-I`, `-isystem`, `-iquote`, `-idirafter`,
/// `-include`, joined or with a separate value), which name the user's and simjig's
/// directories rather than the toolchain's.
fn without_include_flags(args: &[std::ffi::OsString]) -> Vec<&std::ffi::OsStr> {
    const OPTS: &[&str] = &["-isystem", "-iquote", "-idirafter", "-include", "-I"];
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let text = a.to_string_lossy();
        match OPTS.iter().find(|o| text.starts_with(**o)) {
            Some(opt) if text.len() == opt.len() => {
                it.next();
            }
            Some(_) => {}
            None => out.push(a.as_os_str()),
        }
    }
    out
}

/// The directories between `#include <...> search starts here:` and `End of search
/// list.` in the stderr of `-E -v` (GCC and Clang), without Clang's ` (framework
/// directory)` suffix.
fn system_search_list(stderr: &str) -> Vec<PathBuf> {
    stderr
        .lines()
        .skip_while(|l| !l.starts_with("#include <...> search starts here:"))
        .skip(1)
        .take_while(|l| !l.starts_with("End of search list."))
        .map(|l| l.trim().trim_end_matches(" (framework directory)"))
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
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

/// The cached archive for `pre`, if the manifest's dependencies still hash to an entry;
/// else their current digests, if there is a manifest, for the compile's snapshot.
fn lookup(
    cache: &Cache,
    spec: &Spec<'_>,
    pre: &str,
    manifest: &Path,
) -> Result<Archive, Option<Digests>> {
    let m = Manifest::read(manifest).ok_or(None)?;
    let current = m.current().ok_or(None)?;
    let full = current.key(DEPS_DOMAIN, pre);
    let name = archive_name(spec.what, &full);
    let dir = cache.root().join(spec.kind).join(&full);
    if !dir.join(format!("lib{name}.a")).is_file() {
        return Err(Some(current));
    }
    Ok(Archive {
        dir,
        name,
        deps: m.paths(),
        changed: None,
    })
}

/// Publishes the archive `lib` that `spec` compiled in `work` as the entry for the full
/// key `full`; returns the entry and the archive's name. The entry holds the archive
/// alone: its objects are inside it.
fn publish(
    cache: &Cache,
    spec: &Spec<'_>,
    full: &str,
    lib: &Path,
    work: &Path,
) -> Result<(PathBuf, String), Error> {
    let name = archive_name(spec.what, full);
    let out = cache.work_dir()?;
    let dst = out.join(format!("lib{name}.a"));
    std::fs::rename(lib, &dst).map_err(Error::io("renaming into", &dst))?;
    cache::remove_best_effort(work);
    let entry = cache.root().join(spec.kind).join(full);
    if entry.is_dir() && !entry.join(format!("lib{name}.a")).is_file() {
        cache::remove_best_effort(&entry);
    }
    Cache::publish(&out, &entry)?;
    Ok((entry, name))
}

/// The name of an archive before its full key is known.
fn provisional_name(what: &str) -> String {
    format!("simjig_{what}")
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
/// `-idirafter` in the archive's flags.
fn search_dirs(i: &Inputs<'_>, spec: &Spec<'_>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = spec
        .sources
        .iter()
        .filter_map(|s| s.parent().map(Path::to_path_buf))
        .collect();
    dirs.extend(spec.includes.iter().cloned());
    dirs.extend(i.recipe.include_dirs.iter().cloned());
    dirs.extend(flag_dirs(spec.flags));
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
pub(crate) fn hash_listing(h: &mut KeyHasher, dir: &Path) {
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
    fn the_system_search_list_is_read_from_the_compilers_report() {
        let clang = "clang version 21\n#include \"...\" search starts here:\n/q\n\
                     #include <...> search starts here:\n /p/bin/../include/c++/v1\n \
                     /sdk/usr/include\n /sdk/System/Library/Frameworks (framework \
                     directory)\nEnd of search list.\n# 1 \"<stdin>\"\n";
        assert_eq!(
            system_search_list(clang),
            [
                "/p/bin/../include/c++/v1",
                "/sdk/usr/include",
                "/sdk/System/Library/Frameworks"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(system_search_list("no -v here"), Vec::<PathBuf>::new());
    }

    /// Clang in a conda prefix on macOS: the prefix's `include/` (other packages'
    /// headers, which the conda clang's configuration adds with `-isystem`) is not the
    /// toolchain's, and an upgrade of a header there must rerun the build script.
    #[test]
    fn the_toolchain_is_the_resource_dir_the_cxx_library_and_the_sdk() {
        let clang = "clang version 21\n \"/p/bin/clang-21\" -cc1 -triple arm64 \
                     -resource-dir /p/lib/clang/21 -isystem /p/bin/../include -isysroot \
                     /sdk/MacOSX.sdk -internal-isystem /p/bin/../include/c++/v1 -x c++ -\n\
                     #include <...> search starts here:\n /p/bin/../include\n \
                     /p/bin/../include/c++/v1\n /p/lib/clang/21/include\n \
                     /sdk/MacOSX.sdk/usr/include\n /sdk/MacOSX.sdk/System/Library/Frameworks \
                     (framework directory)\nEnd of search list.\n";
        assert_eq!(
            toolchain_from(
                clang,
                "/p/lib/clang/21\n",
                "/p/lib/clang/21/include\n",
                Some("/sdk/MacOSX.sdk\n")
            ),
            [
                "/p/bin/../include/c++/v1",
                "/p/lib/clang/21",
                "/sdk/MacOSX.sdk"
            ]
            .map(PathBuf::from)
        );
    }

    /// GCC on Linux: `/usr/local/include` and `/usr/include` (libc and every `-dev`
    /// package's headers) are in the search list but not the toolchain's; GCC has no
    /// `-print-resource-dir` (an error, so no stdout).
    #[test]
    fn gccs_report_gives_its_own_dirs_and_not_usr_include() {
        let gcc = "Using built-in specs.\nCOLLECT_GCC=g++\n \
                   /usr/libexec/gcc/x86_64-linux-gnu/13/cc1plus -E -quiet -v -imultiarch \
                   x86_64-linux-gnu -D_GNU_SOURCE - -mtune=generic -march=x86-64\n\
                   ignoring nonexistent directory \"/usr/local/include/x86_64-linux-gnu\"\n\
                   #include \"...\" search starts here:\n\
                   #include <...> search starts here:\n /usr/include/c++/13\n \
                   /usr/include/x86_64-linux-gnu/c++/13\n /usr/include/c++/13/backward\n \
                   /usr/lib/gcc/x86_64-linux-gnu/13/include\n /usr/local/include\n \
                   /usr/include/x86_64-linux-gnu\n /usr/include\nEnd of search list.\n\
                   COLLECT_GCC_OPTIONS='-E' '-v' '-shared-libgcc' '-mtune=generic'\n";
        let include = "/usr/lib/gcc/x86_64-linux-gnu/13/include\n";
        assert_eq!(
            toolchain_from(gcc, "", include, None),
            [
                "/usr/include/c++/13",
                "/usr/include/c++/13/backward",
                "/usr/include/x86_64-linux-gnu/c++/13",
                "/usr/lib/gcc/x86_64-linux-gnu/13",
            ]
            .map(PathBuf::from)
        );
        // A GCC without its own headers prints the name back; a cross GCC's `-isysroot`
        // (quoted in `COLLECT_GCC_OPTIONS`) is its SDK.
        assert_eq!(
            toolchain_from(
                "COLLECT_GCC_OPTIONS='-E' '-isysroot' '/sr'",
                "",
                "include",
                None
            ),
            [PathBuf::from("/sr")]
        );
    }

    /// A `c++` directory of the search list is the toolchain's only under the compiler's
    /// prefix or the SDK: one from the user's `CPLUS_INCLUDE_PATH` is not. An
    /// `-print-file-name=include` answer counts only when the search list has it, so a
    /// compiler printing `/opt/include` does not make all of `/opt` the toolchain's.
    #[test]
    fn user_cxx_dirs_and_unsearched_include_answers_are_not_the_toolchains() {
        let clang = "clang version 21\nInstalledDir: /p/bin\n\
                     #include <...> search starts here:\n /home/u/libs/c++/include\n \
                     /p/bin/../include/c++/v1\n /p/lib/clang/21/include\n \
                     /sdk/usr/include/c++/v1\n /sdk/usr/include\nEnd of search list.\n";
        assert_eq!(
            toolchain_from(clang, "", "/p/lib/clang/21/include\n", Some("/sdk\n")),
            [
                "/p/bin/../include/c++/v1",
                "/p/lib/clang/21",
                "/sdk",
                "/sdk/usr/include/c++/v1"
            ]
            .map(PathBuf::from)
        );
        let gcc = "#include <...> search starts here:\n /usr/include/c++/13\n \
                   /usr/include\nEnd of search list.\n";
        assert_eq!(
            toolchain_from(gcc, "", "/opt/include\n", None),
            Vec::<PathBuf>::new(),
            "neither /opt nor, without a prefix, /usr/include/c++/13"
        );
    }

    #[test]
    fn toolchain_dirs_hold_its_headers_however_they_are_spelled() {
        let dirs = [
            PathBuf::from("/p/include"),
            PathBuf::from("/sdk/usr/include"),
        ];
        assert!(under_any(
            Path::new("/p/bin/../include/c++/v1/vector"),
            &dirs
        ));
        assert!(under_any(Path::new("/sdk/usr/include/./stdio.h"), &dirs));
        assert!(!under_any(
            Path::new("/p/share/verilator/include/verilated.h"),
            &dirs
        ));
        assert!(!under_any(Path::new("/work/cpp/hook.h"), &dirs));
        assert!(
            !under_any(Path::new("/p/includes/x.h"), &dirs),
            "whole components"
        );
    }

    #[test]
    fn the_probe_drops_include_path_flags_and_keeps_the_rest() {
        let args: Vec<std::ffi::OsString> = [
            "-O2",
            "-I/rt",
            "-I",
            "/vl",
            "-isystem",
            "/p/include",
            "-include",
            "cfg.h",
            "-isysroot",
            "/sdk",
            "--target=x",
        ]
        .map(Into::into)
        .into();
        assert_eq!(
            without_include_flags(&args),
            ["-O2", "-isysroot", "/sdk", "--target=x"]
        );
    }

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
