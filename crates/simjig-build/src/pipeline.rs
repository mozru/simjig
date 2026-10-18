//! The steps of [`crate::Dut::build_in`] around the emitter: claiming the model prefix,
//! getting the type IR (one JSON pass, or two for a DUT with interface ports), and
//! linting the layout check.

use std::path::{Path, PathBuf};

use svtypes::ir::{Design, Module, ParamValue, VariableKind};

use crate::cache::Cache;
use crate::codegen;
use crate::dut::{DutConfig, GeneratedInput};
use crate::emit::SvModule;
use crate::hash::KeyHasher;
use crate::verilator::{self, Verilator};
use crate::{Error, iface_diag};

/// Makes sure no two configurations share a model prefix across the crates sharing the
/// cache.
///
/// Why: two models with the same `--prefix` define the same C++ symbols, and a static
/// link silently keeps one copy (88 of 88 strong symbols collide for the counter;
/// docs/notes/spikes.md), so one model would run the other's code. The prefix is 32 bits
/// of [`DutConfig::model_digest`]; `prefixes/<scheme>/<prefix>` records the full digest
/// of the configuration that first used the prefix, and another digest with the same
/// prefix (a collision of the 32 bits) is an error. Within one build script, a DUT name
/// may be built again only with the same configuration ([`crate::script::claim_name`]).
///
/// The record is the digest itself, so a configuration that changes the model always
/// changes its prefix too, and a claim a failed build left behind is never mistaken for
/// a collision. `<scheme>` ([`identity_scheme`]) is the simjig-build version.
pub(crate) fn claim_prefix(cache: &Cache, config: &DutConfig, prefix: &str) -> Result<(), Error> {
    let identity = config.model_digest();
    match cache.claim_prefix(&identity_scheme(), prefix, &identity)? {
        None => Ok(()),
        Some(other) => Err(Error::Config {
            dut: config.name.clone(),
            msg: format!(
                "model prefix `{prefix}` is already used by a different configuration \
                 (identity {other}, this one {identity}); rename the DUT"
            ),
        }),
    }
}

/// The claims' namespace: the simjig-build version (whose `model_digest` may differ).
fn identity_scheme() -> String {
    let mut h = KeyHasher::new("simjig-dut-identity-scheme-v2");
    h.str(env!("CARGO_PKG_VERSION"));
    h.finish()[..16].to_owned()
}

/// The IR the emitter sees, and what the model build needs from getting it.
#[derive(Debug)]
pub(crate) struct Ir {
    /// The IR of the model top.
    pub(crate) design: Design,
    /// The generated wrapper source, for a DUT with interface ports.
    pub(crate) wrapper: Option<PathBuf>,
    /// The `--top-module` of the model build.
    pub(crate) top: String,
}

/// Runs the JSON pass, or for a DUT with interface ports the two-pass flow
/// (docs/notes/codegen.md, "Build flow"): Verilator rejects interface ports on the top,
/// in `--json-only` too, so
/// 1. a generated elaboration top instantiates the DUT, and its IR gives the wrapper
///    plan ([`codegen::lower_wrapper`]);
/// 2. the generated wrapper is the top of the second pass and of the model, and its IR
///    is the one the emitter sees.
///
/// DUT parameters go into the generated tops, not `-G`, which would reach the top.
///
/// `generated` are the stored user-generated design inputs (`DutConfig::sv_inputs`).
pub(crate) fn design_ir(
    config: &DutConfig,
    top: &str,
    generated: &[PathBuf],
    verilator: &Verilator,
    cache: &Cache,
) -> Result<Ir, Error> {
    let json_pass = |top: &str, extra: &[PathBuf]| {
        svtypes::verilator::json_pass(&config.json_pass_opts(top, generated, extra, verilator))
            .map_err(|e| {
                iface_diag::unbound_iface_ports(config, &e)
                    .or_else(|| iface_diag::binding_mistakes(config, &e))
                    .or_else(|| unknown_params(config, generated, verilator, cache, &e))
                    .unwrap_or_else(|| Error::json_pass(&config.name, e, extra))
            })
    };
    let Some(mut iface) = config.iface_top() else {
        return Ok(Ir {
            design: json_pass(top, &[])?,
            wrapper: None,
            top: top.to_owned(),
        });
    };
    let codegen_err = |source| Error::codegen(&config.name, source);
    let elab_ir = elaborate(config, &mut iface, cache, &json_pass)?;
    let plan = codegen::lower_wrapper(&elab_ir, &iface).map_err(codegen_err)?;
    let wrapper_top = codegen::wrapper_module(top);
    let wrapper = cache.store_input(&GeneratedInput {
        file_name: format!("{wrapper_top}.sv"),
        contents: codegen::emit_wrapper(&plan),
    })?;
    let design = json_pass(&wrapper_top, std::slice::from_ref(&wrapper))?;
    Ok(Ir {
        design,
        wrapper: Some(wrapper),
        top: wrapper_top,
    })
}

/// The elaboration top's IR for `iface`, after tying the interfaces' parameters to the
/// DUT's: a [`crate::Dut::param`] that a bound interface also declares (`WIDTH` of
/// both) and its binding leaves unset is passed to the interface too, costing one more
/// JSON pass when there is one; a binding that sets it to another value is refused.
///
/// Why: the two are one quantity in practice, and a mismatch is a width mismatch at the
/// port, which only lint reports: under `-Wno-fatal` the model built and silently
/// extended or truncated (checked against 5.052: a 12-bit `q = up.d` from an 8-bit
/// interface). The rule is by name, so an interface parameter that merely shares a
/// module parameter's name cannot be set apart from it.
fn elaborate(
    config: &DutConfig,
    iface: &mut codegen::IfaceTop,
    cache: &Cache,
    json_pass: &impl Fn(&str, &[PathBuf]) -> Result<Design, Error>,
) -> Result<Design, Error> {
    let elab_top = codegen::elab_module(&iface.dut);
    let pass = |iface: &codegen::IfaceTop| {
        let elab = cache.store_input(&GeneratedInput {
            file_name: format!("{elab_top}.sv"),
            contents: codegen::emit_elab_top(iface),
        })?;
        json_pass(&elab_top, &[elab])
    };
    let mut design = pass(iface)?;
    if tie_iface_params(&design, iface) {
        design = pass(iface)?;
    }
    match iface_param_mismatches(&design, iface) {
        lines if lines.is_empty() => Ok(design),
        lines => Err(Error::Config {
            dut: config.name.clone(),
            msg: lines.join("\n"),
        }),
    }
}

/// The DUT module in the elaboration IR `design`.
fn dut_in<'d>(design: &'d Design, iface: &codegen::IfaceTop) -> Option<&'d Module> {
    design.modules.iter().find(|m| m.orig_name == iface.dut)
}

/// The interface the binding of `port` instantiates in `design`.
fn bound<'d>(design: &'d Design, dut: &Module, port: &str) -> Option<&'d Module> {
    let p = dut.iface_ports.iter().find(|p| p.name == port)?;
    Some(design.module(p.interface))
}

/// Adds to each binding the DUT parameters its interface declares and it leaves unset
/// (see [`elaborate`]); whether it added any.
fn tie_iface_params(design: &Design, iface: &mut codegen::IfaceTop) -> bool {
    let Some(dut) = dut_in(design, iface) else {
        return false;
    };
    let codegen::IfaceTop {
        params, bindings, ..
    } = iface;
    let mut tied = false;
    for b in bindings {
        let Some(unit) = bound(design, dut, &b.port) else {
            continue;
        };
        for (name, value) in params.iter() {
            if declares(unit, name) && !b.params.iter().any(|(n, _)| n == name) {
                b.params.push((name.clone(), value.clone()));
                tied = true;
            }
        }
    }
    tied
}

/// Whether `unit` has an overridable parameter `name`.
fn declares(unit: &Module, name: &str) -> bool {
    unit.params.iter().any(|p| p.name == name)
        || unit.unrepresentable.iter().any(|p| p.name == name)
}

/// One line per binding parameter that elaborated to another value than the DUT's
/// same-named [`crate::Dut::param`] (see [`elaborate`]).
fn iface_param_mismatches(design: &Design, iface: &codegen::IfaceTop) -> Vec<String> {
    let Some(dut) = dut_in(design, iface) else {
        return Vec::new();
    };
    let value = |unit: &Module, name: &str| {
        unit.params
            .iter()
            .find(|p| p.name == name)
            .map(|p| shown(&p.value))
    };
    let mut lines = Vec::new();
    for b in &iface.bindings {
        let Some(unit) = bound(design, dut, &b.port) else {
            continue;
        };
        for (name, set) in &iface.params {
            let (Some(theirs), Some(ours)) = (value(unit, name), value(dut, name)) else {
                continue;
            };
            if theirs != ours {
                lines.push(format!(
                    "interface `{}` (bound to `{}`) has `{name}` = {theirs}, but \
                     `.param(\"{name}\", {set})` makes module `{}`'s `{name}` {ours}; they \
                     must agree (a port of the wrong width is extended or truncated \
                     silently under `-Wno-fatal`): leave `{name}` out of the binding, which \
                     then takes the `.param` value",
                    b.interface, b.port, iface.dut
                ));
            }
        }
    }
    lines
}

/// A parameter value as the user wrote it: an integral one in decimal.
fn shown(value: &ParamValue) -> String {
    match value {
        ParamValue::Bits(l) => l.to_u64().map_or_else(|| l.to_string(), |v| v.to_string()),
        ParamValue::String(text) => format!("{text:?}"),
        ParamValue::Real(text) => text.clone(),
        other => format!("{other:?}"),
    }
}

/// Verilator's diagnostics for a parameter override the design does not have (5.052): a
/// `-G` on the command line, a `#(.NAME(..))` in a generated top (`PINNOTFOUND`), and a
/// `#(.NAME(..))` naming a localparam or a variable (checked for both).
const CMDLINE_PARAM_NOT_FOUND: &str = "Parameters from the command line were not found";
const PARAM_NOT_FOUND: &str = ": Parameter not found: '";
const OVERRIDES_NON_PARAM: &str = ": Instance attempts to override '";

/// When the JSON pass failed because a [`crate::Dut::param`] names no parameter of the
/// top module, or a [`crate::Dut::bind_iface_with_params`] none of its interface, the
/// error that names it, the closest real one and all of them (or says it is a
/// localparam), instead of Verilator's message about the command line or about the
/// generated top.
///
/// Why a second JSON pass: the failing pass wrote no IR, and the parameters are only in
/// the IR. It runs only on this failure path, without the overrides (and for a DUT with
/// interface ports, through the elaboration top, whose IR has the DUT as a submodule).
/// When that pass fails too, or every override exists (the mistake is in the sources),
/// Verilator's error stands.
fn unknown_params(
    config: &DutConfig,
    generated: &[PathBuf],
    verilator: &Verilator,
    cache: &Cache,
    e: &svtypes::error::Error,
) -> Option<Error> {
    let svtypes::error::Error::Tool { stderr, .. } = e else {
        return None;
    };
    // The user's top: the pass that failed may have been through a generated top.
    let top = config.top.as_deref()?;
    let overrides = !config.params.is_empty() || config.ifaces.iter().any(|b| !b.params.is_empty());
    if !overrides
        || ![
            CMDLINE_PARAM_NOT_FOUND,
            PARAM_NOT_FOUND,
            OVERRIDES_NON_PARAM,
        ]
        .iter()
        .any(|d| stderr.contains(d))
    {
        return None;
    }
    let design = bare_ir(config, top, generated, verilator, cache)?;
    let module = design.modules.iter().find(|m| m.orig_name == top)?;
    let mut lines = unknown_in(&format!("module `{top}`"), module, &config.params);
    for b in &config.ifaces {
        let Some(port) = module.iface_ports.iter().find(|p| p.name == b.port) else {
            continue; // a binding mistake, which `iface_diag` explains
        };
        let unit = format!("interface `{}` (bound to `{}`)", b.interface, b.port);
        lines.extend(unknown_in(&unit, design.module(port.interface), &b.params));
    }
    lines.dedup();
    (!lines.is_empty()).then(|| Error::Config {
        dut: config.name.clone(),
        msg: lines.join("\n"),
    })
}

/// The IR of `config` without any parameter override, the DUT's or its interfaces'.
fn bare_ir(
    config: &DutConfig,
    top: &str,
    generated: &[PathBuf],
    verilator: &Verilator,
    cache: &Cache,
) -> Option<Design> {
    let bare = DutConfig {
        params: Vec::new(),
        ifaces: config
            .ifaces
            .iter()
            .map(|b| codegen::IfaceBinding {
                params: Vec::new(),
                ..b.clone()
            })
            .collect(),
        ..config.clone()
    };
    match bare.iface_top() {
        None => svtypes::verilator::json_pass(&bare.json_pass_opts(top, generated, &[], verilator)),
        Some(iface) => {
            let elab_top = codegen::elab_module(top);
            let elab = cache
                .store_input(&GeneratedInput {
                    file_name: format!("{elab_top}.sv"),
                    contents: codegen::emit_elab_top(&iface),
                })
                .ok()?;
            let opts = bare.json_pass_opts(&elab_top, generated, &[elab], verilator);
            svtypes::verilator::json_pass(&opts)
        }
    }
    .ok()
}

/// One line per override in `params` that `unit` (`module` described for the user) has
/// no parameter for.
fn unknown_in(unit: &str, module: &Module, params: &[(String, String)]) -> Vec<String> {
    let known: Vec<&str> = module
        .params
        .iter()
        .map(|p| p.name.as_str())
        .chain(module.unrepresentable.iter().map(|p| p.name.as_str()))
        .collect();
    let it_has = if known.is_empty() {
        "it has no parameters".to_owned()
    } else {
        format!("it has {}", known.join(", "))
    };
    let mut lines: Vec<String> = Vec::new();
    for (name, _) in params.iter().filter(|(n, _)| !known.contains(&n.as_str())) {
        let local = module
            .variables
            .iter()
            .any(|v| v.name == *name && v.kind == VariableKind::Localparam);
        let line = if local {
            format!(
                "{unit} has no parameter `{name}`: `{name}` is a localparam and cannot be overridden ({it_has})"
            )
        } else {
            format!(
                "{unit} has no parameter `{name}`{} ({it_has})",
                codegen::did_you_mean(name, known.iter().copied())
            )
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

/// Lints the layout check with the model's flags, without `-G` (docs/notes/codegen.md).
/// It runs on every build (it is not cached): a `--lint-only` run, small next to the
/// C++ compile.
/// A layout mismatch is an elaboration-time `$fatal`, which 5.052 reports as the
/// `USERFATAL` warning; [`codegen::LAYOUT_CHECK_FLAGS`] makes it an error.
pub(crate) fn layout_check(
    config: &DutConfig,
    generated: &[PathBuf],
    verilator: &Verilator,
    cache: &Cache,
    check: &SvModule,
) -> Result<(), Error> {
    let file = cache.store_input(&GeneratedInput {
        file_name: format!("{}.sv", check.module),
        contents: check.contents.clone(),
    })?;
    let mut cmd = verilator.command();
    cmd.args(layout_check_args(config, generated, &check.module, &file));
    let what = format!("the layout check of DUT `{}`", config.name);
    verilator::run(&mut cmd, &what).map(|_| ())
}

fn layout_check_args(
    config: &DutConfig,
    generated: &[PathBuf],
    module: &str,
    file: &Path,
) -> Vec<String> {
    // `-Wno-fatal` first: lint policy belongs to the model build, which sees the same
    // sources. Without it, a design with `timeunit` fails here on TIMESCALEMOD, because
    // the generated module declares none. The layout-check flags come last, after the
    // user's arguments, so that none of these can turn a mismatch into a warning (checked
    // against 5.052): a later `-Wwarn-USERFATAL` downgrades an earlier
    // `-Werror-USERFATAL`, and `-Wno-USERFATAL` disables the warning until a later
    // `-Wwarn-USERFATAL` re-enables it (`-Werror-` alone does not).
    let mut a = vec!["-Wno-fatal".to_owned()];
    let sources = generated
        .iter()
        .chain(&config.sources)
        .cloned()
        .chain([file.to_path_buf()])
        .collect();
    a.extend(config.design(module, sources, false).design_args());
    a.push("-Wwarn-USERFATAL".to_owned());
    a.extend(codegen::LAYOUT_CHECK_FLAGS.iter().map(|f| (*f).to_owned()));
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_check_args_have_no_parameter_overrides() {
        let config = DutConfig {
            name: "c".into(),
            sources: vec!["/r/c.sv".into()],
            params: vec![("WIDTH".into(), "12".into())],
            defines: vec![("K".into(), "V".into())],
            top: Some("c".into()),
            ..DutConfig::default()
        };
        let generated = [PathBuf::from("/g/pkg.sv")];
        let args = layout_check_args(&config, &generated, "simjig_layout_c", Path::new("/g/l.sv"));
        assert_eq!(
            args.join(" "),
            "-Wno-fatal --no-timing --top-module simjig_layout_c -DK=V /g/pkg.sv /r/c.sv /g/l.sv \
             -Wwarn-USERFATAL --lint-only -Werror-USERFATAL"
        );
    }

    #[test]
    fn a_claim_from_another_identity_scheme_is_not_a_collision() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().to_path_buf()).unwrap();
        assert_eq!(cache.claim_prefix("old", "Vc_1", "id-a").unwrap(), None);
        // The same configuration as seen by a simjig-build with another `DutConfig`.
        assert_eq!(cache.claim_prefix("new", "Vc_1", "id-b").unwrap(), None);
        // A real collision within one scheme is still found.
        assert_eq!(
            cache.claim_prefix("new", "Vc_1", "id-c").unwrap(),
            Some("id-b".to_owned())
        );
        assert_eq!(identity_scheme().len(), 16);
    }
}
