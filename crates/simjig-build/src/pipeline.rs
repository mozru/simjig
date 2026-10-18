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
                    .unwrap_or_else(|| Error::json_pass(&config.name, e, extra))
            })
    };
    let Some(iface) = config.iface_top() else {
        return Ok(Ir {
            design: json_pass(top, &[])?,
            wrapper: None,
            top: top.to_owned(),
        });
    };
    let codegen_err = |source| Error::codegen(&config.name, source);
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
