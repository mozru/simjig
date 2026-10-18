//! What one build script has built so far, for the checks that span its DUTs.
//!
//! A build script is one process with one `OUT_DIR`, and every model it builds links into
//! the same test binaries. Some conflicts only show when two of those models meet at link
//! time, where a static linker resolves them silently by keeping the first definition:
//! - a DUT name built twice with different configurations (the name also names
//!   `OUT_DIR/simjig/<name>.rs`);
//! - a DPI export name defined by two models: DPI symbols are global C functions
//!   (IEEE 1800-2023 §35.5.1), so every caller would bind to one model's export.
//!
//! The registry is keyed by `OUT_DIR` so tests that build in several sandboxes from one
//! process do not see each other's models.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::dut::DutConfig;
use crate::{BuildEnv, Error};

/// One model this build script built.
#[derive(Debug)]
struct Model {
    out_dir: PathBuf,
    name: String,
    /// The configuration identity (see `pipeline::claim`).
    identity: String,
    prefix: String,
    /// The DPI functions and tasks the model exports, from `<prefix>__Dpi.h`.
    dpi_exports: Vec<String>,
}

static BUILT: Mutex<Vec<Model>> = Mutex::new(Vec::new());

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
pub(crate) fn claim_name(
    env: &BuildEnv,
    config: &DutConfig,
    identity: &str,
    prefix: &str,
) -> Result<(), Error> {
    let mut built = built();
    match built
        .iter()
        .find(|m| m.out_dir == env.out_dir && m.name == config.name)
    {
        Some(m) if m.identity != identity => Err(config_error(
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
                identity: identity.to_owned(),
                prefix: prefix.to_owned(),
                dpi_exports: Vec::new(),
            });
            Ok(())
        }
    }
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
