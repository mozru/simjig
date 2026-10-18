//! The front end's results in the shared cache: the type IR the JSON pass gives
//! (`pipeline::design_ir`) and the layout check's verdict (`pipeline::layout_check`), so a
//! rerun of the build script that changes nothing runs neither.
//!
//! Keys, in two steps like the other caches (`cache`, `archive`):
//! 1. the *pre key* hashes what decides the IR up front: [`crate::ABI_VERSION`], the
//!    `simjig-build` version, the IR schema, the Verilator (version line, executable,
//!    environment), every argument of the first JSON pass, the contents of the files they
//!    name, the file names in the include directories, and for a DUT with interface ports
//!    its generated elaboration top and bindings;
//! 2. the manifest `manifests/<pre key>` lists the files Verilator read, as the model's
//!    verilation reports them: the same sources and include directories as the JSON
//!    passes, with the same defines and timing (one builder, `DutConfig::design`). The
//!    generated inputs among them (`inputs/`) are left out: they derive from the pre key,
//!    or are the model's own `.vlt`. The *full key* adds their contents.
//!
//! Entries: `fronts/<full key>` holds the IR, the model top and the generated wrapper
//! (stored again on a hit, so a cache that lost `inputs/` still works); `checks/<key>`
//! marks a layout check that passed, keyed on the full key and the check's text, which
//! the emitter may change without changing the IR.
//!
//! A front entry is recorded only after the model has been verilated, the first time
//! the deps are known; until then, every build runs the JSON pass. Nor is it recorded
//! when a dependency changed between the lookup and the end of the verilation (see
//! `manifest`): the IR may be of the old contents.

use std::path::{Path, PathBuf};

use crate::cache::Cache;
use crate::dut::{DutConfig, GeneratedInput};
use crate::emit::SvModule;
use crate::hash::{self, KeyHasher};
use crate::manifest::{Manifest, Recorded, Snapshot};
use crate::pipeline::Ir;
use crate::verilator::Verilator;
use crate::{ABI_VERSION, Error, codegen};

const DEPS_DOMAIN: &str = "simjig-front-deps-v1";

/// One DUT's front-end lookup.
pub(crate) struct Front {
    pre: String,
    /// Taken before the JSON pass and the layout check ran.
    snapshot: Snapshot,
    /// The full key of the entry found, if any.
    full: Option<String>,
}

impl Front {
    /// Looks up the IR of `config`; returns it on a hit.
    pub(crate) fn lookup(
        cache: &Cache,
        config: &DutConfig,
        top: &str,
        generated: &[PathBuf],
        verilator: &Verilator,
    ) -> Result<(Self, Option<Ir>), Error> {
        let pre = pre_key(config, top, generated, verilator)?;
        let manifest = cache.root().join("manifests").join(&pre);
        let current = Manifest::read(&manifest).and_then(|m| m.current());
        let full = current.as_ref().map(|c| c.key(DEPS_DOMAIN, &pre));
        let entry = full.as_ref().and_then(|full| read_entry(cache, full));
        // The filesystem's clock: a work directory created now (see `manifest`).
        let work = cache.work_dir()?;
        let snapshot = Snapshot::start(&work, current);
        crate::cache::remove_best_effort(&work);
        let mut front = Self {
            pre,
            snapshot,
            full: None,
        };
        let Some(entry) = entry else {
            return Ok((front, None));
        };
        let wrapper = entry
            .wrapper
            .as_ref()
            .map(|w| cache.store_input(w))
            .transpose()?;
        front.full = full;
        let ir = Ir {
            design: entry.design,
            wrapper,
            top: entry.top,
        };
        Ok((front, Some(ir)))
    }

    /// Whether the layout check `check` passed for this IR before.
    pub(crate) fn checked(&self, cache: &Cache, check: &SvModule) -> bool {
        self.full
            .as_ref()
            .is_some_and(|full| check_marker(cache, full, check).is_file())
    }

    /// Records the IR (on a miss) and that `check` passed, once the model's verilation
    /// has read `deps`. Records nothing, and returns the dependency, when one changed
    /// since the lookup or vanished (see `manifest`): the next build runs the JSON pass
    /// (or the layout check) again.
    pub(crate) fn record(
        self,
        cache: &Cache,
        ir: &Ir,
        check: Option<&SvModule>,
        deps: &[PathBuf],
    ) -> Result<Option<PathBuf>, Error> {
        let check = check.filter(|c| !self.checked(cache, c));
        if self.full.is_some() && check.is_none() {
            return Ok(None);
        }
        let inputs = cache.root().join("inputs");
        let deps: Vec<PathBuf> = deps
            .iter()
            .filter(|d| !d.starts_with(&inputs) && !crate::cache::is_verilator_binary(d))
            .cloned()
            .collect();
        let m = match Manifest::record(&deps, &self.snapshot) {
            Recorded::Stable(m) => m,
            Recorded::Changed(path) => return Ok(Some(path)),
        };
        let full = if let Some(full) = self.full {
            full
        } else {
            let full = m.recorded_key(DEPS_DOMAIN, &self.pre);
            write_entry(cache, &full, ir)?;
            let manifest = cache.root().join("manifests").join(&self.pre);
            cache.write_atomic(&manifest, &m.text())?;
            full
        };
        if let Some(check) = check {
            cache.write_atomic(&check_marker(cache, &full, check), "")?;
        }
        Ok(None)
    }
}

fn pre_key(
    config: &DutConfig,
    top: &str,
    generated: &[PathBuf],
    verilator: &Verilator,
) -> Result<String, Error> {
    let mut h = KeyHasher::new("simjig-front-v1");
    h.str(&ABI_VERSION.to_string())
        .str(env!("CARGO_PKG_VERSION"))
        .str(&svtypes::ir::SCHEMA.to_string())
        .str(&verilator.version_line)
        .path(&verilator.exe);
    for (k, v) in verilator.child_env() {
        h.field(k.as_encoded_bytes()).field(v.as_encoded_bytes());
    }
    let args = config
        .json_pass_opts(top, generated, &[], verilator)
        .design_args();
    for a in &args {
        h.str(a);
    }
    for a in &args {
        let p = Path::new(a);
        if p.is_absolute() && p.is_file() {
            h.file(p)?;
        }
    }
    for dir in &config.include_dirs {
        crate::archive::hash_listing(&mut h, dir);
    }
    match config.iface_top() {
        None => h.str("no-iface-top"),
        Some(t) => hash::iface_top(&mut h, &t).str(&codegen::emit_elab_top(&t)),
    };
    Ok(h.finish())
}

/// What `fronts/<full key>` holds.
struct Entry {
    design: svtypes::ir::Design,
    top: String,
    wrapper: Option<GeneratedInput>,
}

fn entry_path(cache: &Cache, full: &str) -> PathBuf {
    cache.root().join("fronts").join(full)
}

fn check_marker(cache: &Cache, full: &str, check: &SvModule) -> PathBuf {
    let mut h = KeyHasher::new("simjig-layout-check-v1");
    h.str(full).str(&check.module).str(&check.contents);
    cache.root().join("checks").join(h.finish())
}

/// `None` for a missing entry, or one this version cannot read (a miss).
fn read_entry(cache: &Cache, full: &str) -> Option<Entry> {
    let text = std::fs::read_to_string(entry_path(cache, full)).ok()?;
    let mut v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let wrapper = match v.get("wrapper")? {
        serde_json::Value::Null => None,
        w => Some(GeneratedInput {
            file_name: w.get("file_name")?.as_str()?.to_owned(),
            contents: w.get("contents")?.as_str()?.to_owned(),
        }),
    };
    Some(Entry {
        top: v.get("top")?.as_str()?.to_owned(),
        design: serde_json::from_value(v.get_mut("design")?.take()).ok()?,
        wrapper,
    })
}

fn write_entry(cache: &Cache, full: &str, ir: &Ir) -> Result<(), Error> {
    let wrapper = match &ir.wrapper {
        None => serde_json::Value::Null,
        Some(path) => {
            let contents = std::fs::read_to_string(path).map_err(Error::io("reading", path))?;
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            serde_json::json!({ "file_name": file_name, "contents": contents })
        }
    };
    let entry = serde_json::json!({
        "design": ir.design,
        "top": ir.top,
        "wrapper": wrapper,
    });
    cache.write_atomic(&entry_path(cache, full), &entry.to_string())
}
