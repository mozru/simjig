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
//! | [`Emitted::shim_cpp`] | cache `inputs/<hash>/<prefix>_shim.cpp` | compiled into the model archive |
//! | [`Emitted::vlt`] | cache `inputs/<hash>/<prefix>.vlt` | Verilator control file |
//! | [`Emitted::layout_check`] | cache `inputs/<hash>/<module>.sv` | `--lint-only` run before verilating |
//!
//! A build that fails replaces `OUT_DIR/simjig/<name>.rs` with a stub (`stub_failed`)
//! whose expansion is a `compile_error!`: the file may be left from an earlier build, or
//! written by this one before a later step failed, and including it would compile against
//! a model that is not linked (a link error naming a vtable symbol). A successful build
//! sets the `cfg` `simjig_dut_<name>` (`cfg_directives`), the documented gate for an
//! optional DUT.
//!
//! The shim is content-addressed like the Verilator inputs, so its path, which the model
//! archive's key and debug information carry, is the same in every target directory:
//! crates (and target directories) that build the same configuration share one model
//! archive.
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
use crate::cache::Cache;
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
    /// The probes the shim binds (`SIMJIG_PROBE`). After verilating, the pipeline checks
    /// each against Verilator's scope table (width, packing, access), so a mismatch fails
    /// the build rather than the first `create`.
    pub probes: Vec<ProbeSite>,
}

/// A probe as the shim binds it, for the build-time check against the scope table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSite {
    /// The probe's path as declared (`counter.u_core.state`), for messages.
    pub path: String,
    /// The declaring module, for messages.
    pub module: String,
    /// The scope as Verilator names it, without the model's instance name
    /// ([`codegen::SignalTarget::Probe::scope`]).
    pub scope: String,
    /// The variable.
    pub var: String,
    /// The bits the shim binds.
    pub width: u32,
    /// Bound for writing (`public_flat_rw`).
    pub writable: bool,
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
    let fail = |source| Error::codegen(&config.name, source);
    let lowered = codegen::lower(design, &config.codegen_config()).map_err(fail)?;
    let features = config.features();
    let opts = ShimOptions {
        prefix: config.prefix(),
        timing: features.timing,
        trace_fst: features.trace_fst,
        coverage: features.coverage,
    };
    let dpi = config.rust_dpi_imports(design)?;
    let rust = RustOptions {
        prefix: opts.prefix.clone(),
        precision_exponent: design.time_precision.0,
        dpi_imports: dpi.iter().map(|i| i.c_name.clone()).collect(),
    };
    Ok(Emitted {
        rust: Some(
            codegen::emit_rust(&lowered, &rust)
                + &codegen::emit_dpi(&dpi, &lowered, &opts.prefix).map_err(fail)?,
        ),
        shim_cpp: Some(codegen::emit_shim(&lowered, &opts).map_err(fail)?),
        vlt: Some(codegen::emit_vlt(&lowered)),
        layout_check: Some(SvModule {
            module: lowered.layout.module.clone(),
            contents: codegen::emit_layout_check(&lowered),
        }),
        ports: None,
        probes: probe_sites(&lowered),
    })
}

fn probe_sites(lowered: &codegen::Lowered) -> Vec<ProbeSite> {
    lowered
        .signals
        .iter()
        .filter_map(|s| match &s.target {
            codegen::SignalTarget::Probe { module, var, scope } => Some(ProbeSite {
                path: s.name.clone(),
                module: module.clone(),
                scope: scope.clone(),
                var: var.clone(),
                width: s.width,
                writable: s.access == codegen::Access::PublicRw,
            }),
            codegen::SignalTarget::Port { .. } => None,
        })
        .collect()
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
    /// The probes the shim binds.
    pub(crate) probes: Vec<ProbeSite>,
}

/// Writes the files that live in `OUT_DIR` and the shim, and collects the rest (see the
/// module docs).
pub(crate) fn place(
    emitted: Emitted,
    design: &Design,
    config: &DutConfig,
    out_dir: &Path,
    cache: &Cache,
) -> Result<Placed, Error> {
    let prefix = config.prefix();
    let dir = generated_dir(out_dir);
    let write = |dir: &Path, name: String, text: &str| -> Result<PathBuf, Error> {
        std::fs::create_dir_all(dir).map_err(Error::io("creating", dir))?;
        let path = dir.join(name);
        write_if_changed(&path, text)?;
        Ok(path)
    };
    let rust = emitted
        .rust
        .map(|r| write(&dir, format!("{}.rs", config.name), &r))
        .transpose()?;
    let model_sources: Vec<PathBuf> = emitted
        .shim_cpp
        .map(|contents| {
            cache.store_input(&GeneratedInput {
                file_name: format!("{prefix}_shim.cpp"),
                contents,
            })
        })
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
        probes: emitted.probes,
    })
}

/// `OUT_DIR/simjig/`: the files simjig generates into `OUT_DIR` (see the module docs).
pub(crate) fn generated_dir(out_dir: &Path) -> PathBuf {
    out_dir.join("simjig")
}

/// The `cargo::` lines of the gate `#[cfg(simjig_dut_<name>)]`: always the
/// `rustc-check-cfg` that declares it, and the `rustc-cfg` that sets it when `built`.
///
/// Why a `cfg` and not a stub check in `include_dut!`: a `cfg` gates every item that
/// names the model (the `include_dut!`, the tests, helpers), which a macro could not
/// do, and Cargo already applies it to every target of the package.
pub(crate) fn cfg_directives(name: &str, built: bool) -> Vec<String> {
    let cfg = format!("simjig_dut_{name}");
    let mut out = vec![format!("cargo::rustc-check-cfg=cfg({cfg})")];
    if built {
        out.push(format!("cargo::rustc-cfg={cfg}"));
    }
    out
}

/// The stub that replaces the generated module of the DUT `name` after a failed build.
fn failed_stub(name: &str, error: &Error) -> String {
    let message = format!(
        "DUT `{name}` failed to build, so it has no bindings and no model (the build \
         script's error or warning has the details):\n{error}\n\
         To build without it, gate its uses on `#[cfg(simjig_dut_{name})]`, which \
         simjig-build sets only when the DUT built."
    );
    // `{:?}` of a `str` is a Rust string literal.
    format!(
        "// @generated by simjig-build: DUT `{name}` failed to build.\n::core::compile_error!({message:?});\n"
    )
}

/// Replaces `OUT_DIR/simjig/<name>.rs` with [`failed_stub`]. Best effort: the build has
/// already failed with `error`, which is what the script reports; a stub that cannot be
/// written leaves the old file, and with it the link error this guards against.
pub(crate) fn stub_failed(out_dir: &Path, name: &str, error: &Error) {
    let dir = generated_dir(out_dir);
    if std::fs::create_dir_all(&dir).is_ok() {
        // Ignored: see above.
        write_if_changed(&dir.join(format!("{name}.rs")), &failed_stub(name, error)).ok();
    }
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

    /// The stub compiles to one `compile_error!` naming the DUT, the error and the gate,
    /// whatever characters the error has.
    #[test]
    fn the_failed_stub_is_a_compile_error_naming_the_dut() {
        let error = Error::Config {
            dut: "opt".into(),
            msg: "no \"sources\"\n\\ here".into(),
        };
        let stub = failed_stub("opt", &error);
        let body = stub.lines().nth(1).unwrap();
        let literal = body
            .strip_prefix("::core::compile_error!(")
            .and_then(|r| r.strip_suffix(");"))
            .unwrap();
        let text: String = serde_json::from_str(literal).unwrap();
        assert!(text.starts_with("DUT `opt` failed to build"), "{text}");
        assert!(text.contains(&error.to_string()), "{text}");
        assert!(text.contains("#[cfg(simjig_dut_opt)]"), "{text}");
    }

    #[test]
    fn the_gate_is_declared_always_and_set_only_when_built() {
        assert_eq!(
            cfg_directives("opt", false),
            ["cargo::rustc-check-cfg=cfg(simjig_dut_opt)"]
        );
        assert_eq!(
            cfg_directives("opt", true),
            [
                "cargo::rustc-check-cfg=cfg(simjig_dut_opt)",
                "cargo::rustc-cfg=simjig_dut_opt"
            ]
        );
    }

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
