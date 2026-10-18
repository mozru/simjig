//! What one build script has built so far, for the checks that span its DUTs.
//!
//! A build script is one process with one `OUT_DIR`, and every model it builds links into
//! the same test binaries. Some conflicts only show when two of those models meet at link
//! time, where a static linker resolves them silently by keeping the first definition:
//! - a DUT name built twice with different configurations (the name also names
//!   `OUT_DIR/simjig/<name>.rs`);
//! - a DPI export name defined by two models: DPI symbols are global C functions
//!   (IEEE 1800-2023 §35.5.1), so every caller would bind to one model's export;
//! - the covergroup runtime compiled with two `VM_COVERAGE` settings (see
//!   [`claim_covergroups`]).
//!
//! It also keeps the link arguments that must appear once per binary: an `-rpath` given
//! twice makes ld64 warn on every link, and FST models ask for one per native library
//! (lz4 and zlib, usually in the same directory) per model ([`once_per_script`]).
//!
//! The registry is keyed by `OUT_DIR` so tests that build in several sandboxes from one
//! process do not see each other's models.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::dut::DutConfig;
use crate::{BuildEnv, Error};

/// One model this build script built.
#[derive(Debug)]
struct Model {
    out_dir: PathBuf,
    name: String,
    /// The whole configuration: the name also names the generated Rust, so a rebuild
    /// must match it in every field, `dpi_in_c` included.
    config: DutConfig,
    prefix: String,
    /// The DPI functions and tasks the model exports, from `<prefix>__Dpi.h`.
    dpi_exports: Vec<String>,
    /// For a model that uses covergroups: whether it was built with `--coverage`.
    covergroup_coverage: Option<bool>,
}

static BUILT: Mutex<Vec<Model>> = Mutex::new(Vec::new());

/// The `-rpath` link arguments already emitted, per `OUT_DIR`.
static RPATHS: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

/// `directives` without the `-rpath` link arguments this build script (`out_dir`) already
/// emitted, for this DUT or an earlier one; every other directive is kept, in order.
pub(crate) fn once_per_script(out_dir: &Path, directives: Vec<String>) -> Vec<String> {
    let mut seen = RPATHS.lock().unwrap_or_else(PoisonError::into_inner);
    directives
        .into_iter()
        .filter(|d| {
            if !d.contains("-rpath,") {
                return true;
            }
            let first = !seen.iter().any(|(dir, arg)| dir == out_dir && arg == d);
            if first {
                seen.push((out_dir.to_path_buf(), d.clone()));
            }
            first
        })
        .collect()
}

fn built() -> MutexGuard<'static, Vec<Model>> {
    // A panic while holding the lock leaves the list itself consistent.
    BUILT.lock().unwrap_or_else(PoisonError::into_inner)
}

fn config_error(config: &DutConfig, msg: String) -> Error {
    Error::Config {
        dut: config.name.clone(),
        msg,
    }
}

/// Records the DUT, or checks that a DUT of the same name had the same configuration.
/// Two names that differ only in case are refused: they name one generated file on a
/// case-insensitive filesystem (macOS and Windows by default), so the second would
/// overwrite the first's module, and the build would differ between machines.
pub(crate) fn claim_name(env: &BuildEnv, config: &DutConfig, prefix: &str) -> Result<(), Error> {
    let mut built = built();
    if let Some(m) = built.iter().find(|m| {
        m.out_dir == env.out_dir
            && m.name != config.name
            && m.name.eq_ignore_ascii_case(&config.name)
    }) {
        return Err(config_error(
            config,
            format!(
                "this build script already built a DUT named `{}`, which differs only in \
                 case: both would generate `OUT_DIR/simjig/{}.rs` on a case-insensitive \
                 filesystem; rename one",
                m.name, config.name
            ),
        ));
    }
    match built
        .iter()
        .find(|m| m.out_dir == env.out_dir && m.name == config.name)
    {
        Some(m) if m.config != *config => Err(config_error(
            config,
            "a DUT of this name was already built with a different configuration in this \
             build script; give each configuration its own name"
                .to_owned(),
        )),
        Some(_) => Ok(()),
        None => {
            built.push(Model {
                out_dir: env.out_dir.clone(),
                name: config.name.clone(),
                config: config.clone(),
                prefix: prefix.to_owned(),
                dpi_exports: Vec::new(),
                covergroup_coverage: None,
            });
            Ok(())
        }
    }
}

/// Whether this build script (its `OUT_DIR`, `out_dir`) already claimed the DUT name
/// `name`, or one differing only in case (which [`claim_name`] refuses, and whose
/// generated file may be the same one).
pub(crate) fn claimed(out_dir: &Path, name: &str) -> bool {
    built()
        .iter()
        .any(|m| m.out_dir == out_dir && m.name.eq_ignore_ascii_case(name))
}

/// The other models of this build script (not `prefix` itself).
fn others<'a>(
    built: &'a mut [Model],
    env: &'a BuildEnv,
    prefix: &'a str,
) -> impl Iterator<Item = &'a mut Model> {
    built
        .iter_mut()
        .filter(move |m| m.out_dir == env.out_dir && m.prefix != prefix)
}

fn this<'a>(built: &'a mut [Model], env: &BuildEnv, prefix: &str) -> Option<&'a mut Model> {
    built
        .iter_mut()
        .find(|m| m.out_dir == env.out_dir && m.prefix == prefix)
}

/// Records the model's DPI exports; fails if another model of this build script exports
/// one of the same names.
pub(crate) fn claim_dpi_exports(
    env: &BuildEnv,
    config: &DutConfig,
    prefix: &str,
    exports: Vec<String>,
) -> Result<(), Error> {
    let mut built = built();
    for other in others(&mut built, env, prefix) {
        if let Some(name) = exports.iter().find(|e| other.dpi_exports.contains(e)) {
            return Err(config_error(
                config,
                format!(
                    "DPI export `{name}` is also exported by DUT `{}` (model {}) in this build \
                     script. DPI functions are global C symbols, so a binary linking both \
                     models would call one model's `{name}` from the other; rename the \
                     function in one design, or build the two DUTs in different crates",
                    other.name, other.prefix
                ),
            ));
        }
    }
    if let Some(m) = this(&mut built, env, prefix) {
        m.dpi_exports = exports;
    }
    Ok(())
}

/// Records that the model uses covergroups, built with `coverage` (`--coverage`); fails
/// if another model of this build script uses them with the other setting.
///
/// Why: Verilator 5.052 compiles `verilated_covergroup.cpp` per model with the model's
/// `VM_COVERAGE`, and the file's bodies differ by that switch, so the two settings give
/// two archives defining the same symbols. A static link keeps the first one, and one
/// model would run with the other's covergroup runtime.
pub(crate) fn claim_covergroups(
    env: &BuildEnv,
    config: &DutConfig,
    prefix: &str,
    coverage: bool,
) -> Result<(), Error> {
    let mut built = built();
    let clash = others(&mut built, env, prefix)
        .find(|m| m.covergroup_coverage.is_some_and(|c| c != coverage))
        .map(|m| (m.name.clone(), m.prefix.clone()));
    if let Some((name, other)) = clash {
        let on_off = |on: bool| if on { "with" } else { "without" };
        return Err(config_error(
            config,
            format!(
                "this DUT uses covergroups and is built {} `.coverage(true)`, but DUT `{name}` \
                 (model {other}) in this build script uses covergroups and is built {} it. \
                 Verilator compiles its covergroup runtime differently for the two, and only \
                 one copy can be linked; give both DUTs the same coverage setting, or build \
                 them in different crates",
                on_off(coverage),
                on_off(!coverage)
            ),
        ));
    }
    if let Some(m) = this(&mut built, env, prefix) {
        m.covergroup_coverage = Some(coverage);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_rpath_is_emitted_once_per_build_script() {
        let rpath = "cargo::rustc-link-arg=-Wl,-rpath,/conda/lib".to_owned();
        let lib = |n: &str| format!("cargo::rustc-link-lib={n}");
        let first = Path::new("/out/rpath-a");
        assert_eq!(
            once_per_script(
                first,
                vec![lib("lz4"), rpath.clone(), lib("z"), rpath.clone()]
            ),
            [lib("lz4"), rpath.clone(), lib("z")]
        );
        // A second FST model of the same script: the directory is already on the rpath.
        assert_eq!(
            once_per_script(first, vec![lib("z"), rpath.clone()]),
            [lib("z")]
        );
        // Another build script (another OUT_DIR) needs its own.
        let other = Path::new("/out/rpath-b");
        assert_eq!(once_per_script(other, vec![rpath.clone()]), [rpath]);
    }
}
