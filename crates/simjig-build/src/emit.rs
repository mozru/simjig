//! The emitter seam: where code generation plugs into the build pipeline.
//!
//! An [`Emitter`] runs after the JSON pass and before verilation. It receives the type IR
//! of the model top and the [`DutConfig`], and returns [`Emitted`] text. For a DUT with
//! interface ports the model top is the generated wrapper (the pipeline writes it, see
//! `crate::dut`), so the IR the emitter sees is the wrapper's. The pipeline (not the
//! emitter) decides where each output goes, so emitters stay pure functions of their
//! inputs and every output path is fixed in one place:
//!
//! | Output | Written to | Used as |
//! |---|---|---|
//! | [`Emitted::rust`] | `OUT_DIR/simjig/<name>.rs` | `include!`d by the user crate |
//! | [`Emitted::shim_cpp`] | `OUT_DIR/simjig/<prefix>_shim.cpp` | compiled into the model archive |
//! | [`Emitted::vlt`] | cache `inputs/<hash>/<prefix>.vlt` | Verilator control file |
//! | [`Emitted::layout_check`] | cache `inputs/<hash>/<module>.sv` | `--lint-only` run before verilating |
//!
//! The header cross-check compares the generated header with [`Emitted::ports`] when the
//! emitter supplies them, else with the IR top module's ports ([`ir_ports`]).
//!
//! The default is [`codegen_emitter`], the real code generator ([`crate::codegen`]).
//! Why a plain `fn` and not a trait: emitters need no state beyond their two inputs (the
//! prefix is [`DutConfig::prefix`]), and a function pointer keeps [`crate::Dut`] `Clone`
//! and `Debug` without boxing.

use std::path::{Path, PathBuf};

use svtypes::ir::{Design, Direction, TypeKind};
use svtypes::width::{WidthError, packed_storage};

use crate::Error;
use crate::codegen::{self, RustOptions, ShimOptions};
use crate::dut::{DutConfig, GeneratedInput};
use crate::header::{HeaderPort, PortDir};

/// Generates the per-DUT files from the IR. See the module docs for where they go.
///
/// Return [`Error::Emit`] for designs the emitter cannot handle.
pub type Emitter = fn(&Design, &DutConfig) -> Result<Emitted, Error>;

/// What an [`Emitter`] produced. `None` fields produce nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Emitted {
    /// The Rust bindings.
    pub rust: Option<String>,
    /// The C++ shim exporting `simjig_vtable_<prefix>` (see `cpp/simjig_model.h`).
    pub shim_cpp: Option<String>,
    /// A Verilator control file (`public_flat_rd` for probes, lint waivers).
    pub vlt: Option<String>,
    /// A module that fails elaboration (`$fatal`) if Verilator lays a type out
    /// differently from the IR. The pipeline lints it with the model's flags plus
    /// [`codegen::LAYOUT_CHECK_FLAGS`] and without `-G` (its types are spelled with
    /// elaborated widths); a failure stops the build before any model memory is touched.
    pub layout_check: Option<SvModule>,
    /// The ports the model header must declare; `None` means the IR top's ports.
    pub ports: Option<Vec<HeaderPort>>,
}

/// A generated SystemVerilog module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvModule {
    /// The module name (also the file stem), passed as `--top-module`.
    pub module: String,
    /// Its source text.
    pub contents: String,
}

/// The emitter that generates nothing. The pipeline still verilates, cross-checks the
/// header against the IR and compiles; the model just exports no vtable.
pub fn noop_emitter(_design: &Design, _config: &DutConfig) -> Result<Emitted, Error> {
    Ok(Emitted::default())
}

/// The code generator: [`codegen::lower`] once, then the Rust bindings, the shim, the
/// `.vlt` file and the layout check from the same [`codegen::Lowered`].
pub fn codegen_emitter(design: &Design, config: &DutConfig) -> Result<Emitted, Error> {
    let fail = |source| Error::Codegen {
        dut: config.name.clone(),
        source,
    };
    let lowered = codegen::lower(design, &config.codegen_config()).map_err(fail)?;
    let features = config.features();
    let opts = ShimOptions {
        prefix: config.prefix(),
        timing: features.timing,
        trace_fst: features.trace_fst,
        coverage: features.coverage,
    };
    let rust = RustOptions {
        prefix: opts.prefix.clone(),
        precision_exponent: design.time_precision.0,
    };
    Ok(Emitted {
        rust: Some(codegen::emit_rust(&lowered, &rust)),
        shim_cpp: Some(codegen::emit_shim(&lowered, &opts).map_err(fail)?),
        vlt: Some(codegen::emit_vlt(&lowered)),
        layout_check: Some(SvModule {
            module: lowered.layout.module.clone(),
            contents: codegen::emit_layout_check(&lowered),
        }),
        ports: None,
    })
}

/// The emitter [`crate::Dut`] uses unless [`crate::Dut::emitter`] replaces it.
pub const DEFAULT_EMITTER: Emitter = codegen_emitter;

/// The ports Verilator declares with a `VL_*` macro for the IR's top module, in IR order.
///
/// Unpacked-array ports are left out: Verilator declares them as `VlUnpacked<..> &name;`
/// without a macro (see [`crate::parse_header_ports`]). The range is the declared one,
/// normalised to `msb >= lsb` as Verilator prints it, for a plain vector with a
/// non-negative range (`logic [5:2]` is `5,2`); everything else (structs, enums, packed
/// arrays, scalars) is `width-1, 0`.
///
/// # Errors
/// A port whose width has no storage ([`packed_storage`]: width 0).
pub fn ir_ports(design: &Design) -> Result<Vec<HeaderPort>, WidthError> {
    let top = design.top_module();
    let mut ports = Vec::new();
    for p in &top.ports {
        let ty = design.ty(p.ty);
        if matches!(ty.kind, TypeKind::UnpackedArray { .. }) {
            continue;
        }
        let Some(width) = ty.width else { continue };
        let storage =
            packed_storage(width).map_err(|e| WidthError(format!("port `{}`: {}", p.name, e.0)))?;
        let (msb, lsb) = match &ty.kind {
            TypeKind::Bits { range: Some(r), .. } => {
                match (u32::try_from(r.left), u32::try_from(r.right)) {
                    (Ok(l), Ok(r)) => (l.max(r), l.min(r)),
                    _ => (width - 1, 0),
                }
            }
            _ => (width - 1, 0),
        };
        ports.push(HeaderPort {
            name: p.cpp_member.clone().unwrap_or_else(|| p.name.clone()),
            dir: match p.dir {
                Direction::Input => PortDir::In,
                Direction::Output => PortDir::Out,
                Direction::Inout => PortDir::InOut,
            },
            msb,
            lsb,
            storage,
        });
    }
    Ok(ports)
}

/// Emitted outputs placed where the rest of the pipeline consumes them.
#[derive(Debug)]
pub(crate) struct Placed {
    /// `OUT_DIR/simjig/<name>.rs`, when the emitter produced Rust.
    pub(crate) rust: Option<PathBuf>,
    /// Sources to compile with the model (the shim).
    pub(crate) model_sources: Vec<PathBuf>,
    /// Verilator inputs to add before the user's sources (the `.vlt`).
    pub(crate) inputs: Vec<GeneratedInput>,
    /// The layout check to lint.
    pub(crate) layout_check: Option<SvModule>,
    /// The ports the header must declare.
    pub(crate) ports: Vec<HeaderPort>,
}

/// Writes the files that live in `OUT_DIR` and collects the rest (see the module docs).
pub(crate) fn place(
    emitted: Emitted,
    design: &Design,
    config: &DutConfig,
    out_dir: &Path,
) -> Result<Placed, Error> {
    let prefix = config.prefix();
    let dir = out_dir.join("simjig");
    let write = |name: String, text: &str| -> Result<PathBuf, Error> {
        std::fs::create_dir_all(&dir).map_err(Error::io("creating", &dir))?;
        let path = dir.join(name);
        write_if_changed(&path, text)?;
        Ok(path)
    };
    let rust = emitted
        .rust
        .map(|r| write(format!("{}.rs", config.name), &r))
        .transpose()?;
    let model_sources = emitted
        .shim_cpp
        .map(|s| write(format!("{prefix}_shim.cpp"), &s))
        .transpose()?
        .into_iter()
        .collect();
    let inputs = emitted
        .vlt
        .map(|contents| GeneratedInput {
            file_name: format!("{prefix}.vlt"),
            contents,
        })
        .into_iter()
        .collect();
    let ports = match emitted.ports {
        Some(ports) => ports,
        None => ir_ports(design).map_err(|e| Error::Emit {
            dut: config.name.clone(),
            msg: e.0,
        })?,
    };
    Ok(Placed {
        rust,
        model_sources,
        inputs,
        layout_check: emitted.layout_check,
        ports,
    })
}

/// Rewriting an identical file would bump its mtime and make Cargo rebuild whatever
/// `include!`s it.
fn write_if_changed(path: &Path, text: &str) -> Result<(), Error> {
    if std::fs::read(path).is_ok_and(|old| old == text.as_bytes()) {
        return Ok(());
    }
    std::fs::write(path, text).map_err(Error::io("writing", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_if_changed_keeps_the_mtime_of_identical_files() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.rs");
        write_if_changed(&p, "x").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(old)
            .unwrap();
        write_if_changed(&p, "x").unwrap();
        assert_eq!(p.metadata().unwrap().modified().unwrap(), old);
        write_if_changed(&p, "y").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "y");
    }
}
