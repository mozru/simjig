//! The steps of [`crate::Dut::build_in`] around the emitter: claiming the model prefix,
//! getting the type IR (one JSON pass, or two for a DUT with interface ports), and
//! linting the layout check.

use std::path::{Path, PathBuf};

use svtypes::ir::Design;

use crate::cache::Cache;
use crate::codegen;
use crate::dut::{DutConfig, GeneratedInput};
use crate::emit::SvModule;
use crate::hash::KeyHasher;
use crate::verilator::{self, Verilator};
use crate::{BuildEnv, Error, script};

/// Makes sure no two configurations share a model prefix.
///
/// Why: two models with the same `--prefix` define the same C++ symbols, and a static
/// link silently keeps one copy (88 of 88 strong symbols collide for the counter;
/// docs/notes/spikes.md), so one model would run the other's code. Two checks:
/// - in this build script, a DUT name may be built again only with the same
///   configuration ([`script::claim_name`]);
/// - across crates sharing the cache, `prefixes/<prefix>` records the full identity of
///   the configuration that first used the prefix; a different one with the same prefix
///   (a collision of its 32-bit hash) is an error.
pub(crate) fn claim(
    env: &BuildEnv,
    cache: &Cache,
    config: &DutConfig,
    prefix: &str,
) -> Result<(), Error> {
    let identity = identity(config);
    let fail = |msg: String| {
        Err(Error::Config {
            dut: config.name.clone(),
            msg,
        })
    };
    script::claim_name(env, config, &identity, prefix)?;
    match cache.claim_prefix(prefix, &identity)? {
        None => Ok(()),
        Some(other) => fail(format!(
            "model prefix `{prefix}` is already used by a different configuration (identity \
             {other}, this one {identity}); rename the DUT"
        )),
    }
}

/// A hash of the whole configuration: equal exactly when two configurations are equal.
fn identity(config: &DutConfig) -> String {
    let mut h = KeyHasher::new("simjig-dut-identity-v1");
    h.str(&format!("{config:?}"));
    h.finish()
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
                unbound_iface_ports(config, &e).unwrap_or_else(|| Error::json_pass(&config.name, e))
            })
    };
    let Some(iface) = config.iface_top() else {
        return Ok(Ir {
            design: json_pass(top, &[])?,
            wrapper: None,
            top: top.to_owned(),
        });
    };
    let codegen_err = |source| Error::Codegen {
        dut: config.name.clone(),
        source,
    };
    let elab_top = codegen::elab_module(top);
    let elab = cache.store_input(&GeneratedInput {
        file_name: format!("{elab_top}.sv"),
        contents: codegen::emit_elab_top(&iface),
    })?;
    let elab_ir = json_pass(&elab_top, &[elab])?;
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

/// Verilator's diagnostics for an interface port left unconnected (5.052 `V3LinkDot.cpp`,
/// `V3Inst.cpp`; checked for both): with no binding at all the DUT is the top, and each
/// interface port is reported at its declaration; under the elaboration top, a port without
/// a binding is reported by name, and the `PINMISSING` warning before it gives its
/// declaration.
const IFACE_PORT_ON_TOP: &str = ": Unsupported: Interfaced port on top level module";
const IFACE_PORT_OPEN: (&str, &str) = (
    "Interface port '",
    "' is not connected to interface/modport pin expression",
);
const MISSING_PIN: &str = "Instance has missing pin: '";
const PORT_DECL: &str = ": ... Location of port declaration";

/// When the JSON pass failed because interface ports of the DUT have no
/// [`crate::Dut::bind_iface`], the error that lists each one with the call that binds it,
/// instead of Verilator's dump (which ends in an internal error).
///
/// Why from stderr: Verilator elaborates no design whose interface ports are unconnected,
/// so no JSON exists to read them from, and `--lint-only` (which accepts them) writes no
/// JSON. The declaration's source line gives the interface and modport.
fn unbound_iface_ports(config: &DutConfig, e: &svtypes::error::Error) -> Option<Error> {
    let svtypes::error::Error::Tool { stderr, .. } = e else {
        return None;
    };
    let top = config.top.as_deref().unwrap_or_default();
    let mut decls: Vec<(String, &str)> = Vec::new(); // (port, declaration loc) from PINMISSING
    let mut pending: Option<String> = None;
    let mut ports: Vec<(String, Option<String>)> = Vec::new();
    for diag in stderr.lines() {
        if let Some(loc) = diag
            .strip_prefix("%Error-UNSUPPORTED: ")
            .and_then(|d| d.strip_suffix(IFACE_PORT_ON_TOP))
        {
            ports.push(iface_port_at(loc)?);
        } else if let Some((_, rest)) = diag.split_once(MISSING_PIN) {
            pending = rest.strip_suffix('\'').map(str::to_owned);
        } else if let Some(loc) = diag.trim_start().strip_suffix(PORT_DECL)
            && let Some(port) = pending.take()
        {
            decls.push((port, loc));
        } else if let Some((_, rest)) = diag.split_once(IFACE_PORT_OPEN.0)
            && let Some(port) = rest.strip_suffix(IFACE_PORT_OPEN.1)
        {
            let decl = decls.iter().find(|(p, _)| p == port);
            let spec = decl
                .and_then(|(_, loc)| iface_port_at(loc))
                .and_then(|(_, s)| s);
            ports.push((port.to_owned(), spec));
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for (port, spec) in ports {
        if config.ifaces.iter().any(|b| b.port == port) {
            return None; // bound, yet reported: not the mistake this explains
        }
        let line = match spec {
            Some(spec) => format!(
                "port `{port}` of `{top}` is an interface port (`{spec}`); add \
                 `.bind_iface(\"{port}\", \"{spec}\")`"
            ),
            None => format!(
                "port `{port}` of `{top}` is an interface port; add \
                 `.bind_iface(\"{port}\", \"<interface>.<modport>\")`"
            ),
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    (!lines.is_empty()).then(|| Error::Config {
        dut: config.name.clone(),
        msg: lines.join("\n"),
    })
}

/// The port named at `file:line:col` and, when the declaration has the usual
/// `interface[.modport] name` shape on that line, its `interface[.modport]`.
fn iface_port_at(loc: &str) -> Option<(String, Option<String>)> {
    let mut parts = loc.rsplitn(3, ':');
    let col: usize = parts.next()?.parse().ok()?;
    let line: usize = parts.next()?.parse().ok()?;
    let text = std::fs::read_to_string(parts.next()?).ok()?;
    let src = text.lines().nth(line.checked_sub(1)?)?;
    let start = src.char_indices().nth(col.checked_sub(1)?)?.0;
    let port: String = src[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if port.is_empty() {
        return None;
    }
    // `bus_if.src out_bus` or `bus_if.src a, out_bus`: the last word not ending in `,`.
    let spec = src[..start]
        .split_whitespace()
        .rev()
        .find(|w| !w.ends_with(','))
        .filter(|w| {
            *w != "interface" // a generic interface port names no interface
                && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        })
        .map(str::to_owned);
    Some((port, spec))
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
    verilator::run(&mut cmd).map(|_| ())
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
    fn identity_tracks_the_whole_configuration() {
        let a = DutConfig {
            name: "c".into(),
            ..DutConfig::default()
        };
        let b = DutConfig {
            generated: vec![GeneratedInput {
                file_name: "x.vlt".into(),
                contents: String::new(),
            }],
            ..a.clone()
        };
        assert_eq!(identity(&a), identity(&a.clone()));
        assert_ne!(identity(&a), identity(&b));
    }
}
