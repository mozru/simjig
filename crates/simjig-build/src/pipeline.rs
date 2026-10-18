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
            .map_err(|e| {
                hint_width_error(e, config, || {
                    diagnosed_mismatches(config, generated, verilator, cache)
                })
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
    let elab_ir = elaborate(&mut iface, cache, &json_pass)?;
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

/// The elaboration top's IR for `iface`, after defaulting the interfaces' parameters to
/// the DUT's: a [`crate::Dut::param`] that a bound interface also declares (`WIDTH` of
/// both) and its binding leaves unset is passed to the interface too, costing one more
/// JSON pass when there is one. A value the binding sets always wins.
///
/// Why only a default: IEEE 1800-2017 §25.5 interface port headers carry no parameter
/// assignments, so nothing in the design says that a module's `W` and its interface
/// port's `W` are one quantity; the shared name is only a heuristic. A module may read a
/// wider interface's width through the port (`out.W`), and the binding is then the
/// user's only say. Where the heuristic leaves the two apart, an [`IfaceParamMismatch`]
/// says so.
fn elaborate(
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
    let design = pass(iface)?;
    if tie_iface_params(&design, iface) {
        return pass(iface);
    }
    Ok(design)
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

/// A parameter that the DUT module and a bound interface both declare, that neither the
/// binding nor a [`crate::Dut::param`] sets, and whose elaborated values differ (for
/// example, two different defaults). Under `-Wno-fatal` such a pair builds silently, with
/// a port of the wrong width extended or truncated (checked against 5.052); under the
/// default fatal lint it stops the model build at a `WIDTH*` warning inside the generated
/// wrapper. The name tie of [`elaborate`] cannot settle it, because it applies only to a
/// `.param`. An explicit value on either side silences it.
#[derive(Debug)]
pub(crate) struct IfaceParamMismatch {
    binding: codegen::IfaceBinding,
    name: String,
    /// The module's value, then the interface's, as the user would write them.
    ours: String,
    theirs: String,
}

/// The [`IfaceParamMismatch`]es in `design`: the IR the emitter sees (the wrapper's, which
/// holds the DUT and each bound interface specialisation), or the elaboration top's, which
/// holds them too. Read from the IR, so a build whose IR came from the cache finds them too.
pub(crate) fn iface_param_mismatches(
    config: &DutConfig,
    design: &Design,
) -> Vec<IfaceParamMismatch> {
    let Some(iface) = config.iface_top() else {
        return Vec::new();
    };
    let Some(dut) = dut_in(design, &iface) else {
        return Vec::new();
    };
    let value = |unit: &Module, name: &str| {
        unit.params
            .iter()
            .find(|p| p.name == name)
            .map(|p| shown(&p.value))
    };
    let set = |params: &[(String, String)], name: &str| params.iter().any(|(n, _)| n == name);
    let mut found = Vec::new();
    for b in &iface.bindings {
        let Some(unit) = bound(design, dut, &b.port) else {
            continue;
        };
        for p in &dut.params {
            if set(&iface.params, &p.name) || set(&b.params, &p.name) {
                continue;
            }
            let (Some(ours), Some(theirs)) = (value(dut, &p.name), value(unit, &p.name)) else {
                continue;
            };
            if ours != theirs {
                found.push(IfaceParamMismatch {
                    binding: b.clone(),
                    name: p.name.clone(),
                    ours,
                    theirs,
                });
            }
        }
    }
    found
}

impl IfaceParamMismatch {
    /// The `cargo::warning` of a build that succeeded with the mismatch.
    pub(crate) fn warning(&self, config: &DutConfig) -> String {
        let Self {
            binding: b,
            name,
            ours,
            theirs,
        } = self;
        format!(
            "cargo::warning=simjig: DUT `{dut}`: module `{module}` has `{name}` = {ours} but \
             interface `{iface}` (bound to `{port}`) has `{name}` = {theirs}, and neither is \
             set; simjig ties a `.param` to a same-named interface parameter only by name, a \
             heuristic (interface ports carry no parameters, IEEE 1800-2017 §25.5). If they \
             are one quantity, {settle}; \
             setting either explicitly silences this",
            dut = config.name,
            module = config.top.as_deref().unwrap_or_default(),
            iface = b.interface,
            port = b.port,
            settle = self.settle(),
        )
    }

    /// The `hint:` of a build that stopped on a width diagnostic, which the mismatch may
    /// explain: Verilator reports it inside the generated wrapper, not at the defaults.
    pub(crate) fn hint(&self, config: &DutConfig) -> String {
        let Self {
            binding: b,
            name,
            ours,
            theirs,
        } = self;
        format!(
            "the width error may come from interface `{iface}` (bound to `{port}`) \
             defaulting `{name}` to {theirs} while `{module}` defaults it to {ours}; if they \
             are one quantity, {settle}",
            iface = b.interface,
            port = b.port,
            module = config.top.as_deref().unwrap_or_default(),
            settle = self.settle(),
        )
    }

    /// The ways to settle the mismatch: one value for both, or the interface's own value
    /// bound explicitly, which changes no width.
    fn settle(&self) -> String {
        let Self {
            binding: b,
            name,
            ours,
            theirs,
        } = self;
        let spec = match &b.modport {
            Some(m) => format!("{}.{m}", b.interface),
            None => b.interface.clone(),
        };
        let port = &b.port;
        format!(
            "set it with `.bind_iface_with_params(\"{port}\", \
             \"{spec}\", [(\"{name}\", {ours})])` or `.param(\"{name}\", {ours})`, which \
             reaches both; if the interface's {theirs} is intended, bind it: \
             `.bind_iface_with_params(\"{port}\", \"{spec}\", [(\"{name}\", {theirs})])` \
             changes no width"
        )
    }
}

/// Verilator 5.052's width warnings (`V3Error.h`): `WIDTH` and the codes it was split
/// into.
const WIDTH_CODES: [&str; 5] = [
    "WIDTH",
    "WIDTHCONCAT",
    "WIDTHEXPAND",
    "WIDTHTRUNC",
    "WIDTHXZEXPAND",
];

/// `e` with a leading `hint:` per mismatch when it is a Verilator run that stopped on a
/// width diagnostic (a fatal `%Warning-WIDTH*`, or an `%Error-WIDTH*` from `-Werror-`).
/// `mismatches` is called only then.
pub(crate) fn hint_width_error(
    mut e: Error,
    config: &DutConfig,
    mismatches: impl FnOnce() -> Vec<IfaceParamMismatch>,
) -> Error {
    if let Error::Command { stderr, hints, .. } = &mut e
        && hints.is_empty()
        && config.iface_top().is_some()
        && has_width_diagnostic(stderr)
    {
        **hints = mismatches().iter().map(|m| m.hint(config)).collect();
    }
    e
}

/// Whether Verilator's `stderr` reports one of [`WIDTH_CODES`].
fn has_width_diagnostic(stderr: &str) -> bool {
    stderr.lines().any(|l| {
        l.strip_prefix("%Warning-")
            .or_else(|| l.strip_prefix("%Error-"))
            .and_then(|r| r.split_once(':'))
            .is_some_and(|(code, _)| WIDTH_CODES.contains(&code))
    })
}

/// The mismatches of a DUT whose JSON pass failed, for [`hint_width_error`]: the failed
/// pass wrote no IR, so this reruns the elaboration top (the DUT and its interfaces as
/// configured) with the width codes downgraded to warnings, after the user's arguments
/// (a later `-Wwarn-<CODE>` overrides an earlier `-Werror-<CODE>`, checked against 5.052),
/// so a `-Werror-WIDTHTRUNC` does not stop it too. Only on this failure path.
fn diagnosed_mismatches(
    config: &DutConfig,
    generated: &[PathBuf],
    verilator: &Verilator,
    cache: &Cache,
) -> Vec<IfaceParamMismatch> {
    let Some(iface) = config.iface_top() else {
        return Vec::new();
    };
    let elab_top = codegen::elab_module(&iface.dut);
    let Ok(elab) = cache.store_input(&GeneratedInput {
        file_name: format!("{elab_top}.sv"),
        contents: codegen::emit_elab_top(&iface),
    }) else {
        return Vec::new();
    };
    let mut opts = config.json_pass_opts(&elab_top, generated, &[elab], verilator);
    opts.extra_args
        .extend(WIDTH_CODES.iter().map(|c| format!("-Wwarn-{c}")));
    svtypes::verilator::json_pass(&opts)
        .map(|design| iface_param_mismatches(config, &design))
        .unwrap_or_default()
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
