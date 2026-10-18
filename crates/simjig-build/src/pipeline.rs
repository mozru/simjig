//! The steps of [`crate::Dut::build_in`] around the emitter: claiming the model prefix,
//! getting the type IR (one JSON pass, or two for a DUT with interface ports), and
//! linting the layout check.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use svtypes::ir::Design;

use crate::cache::Cache;
use crate::codegen;
use crate::dut::{DutConfig, GeneratedInput};
use crate::emit::SvModule;
use crate::hash::KeyHasher;
use crate::verilator::{self, Verilator};
use crate::{BuildEnv, Error};

/// Every DUT built by this process (one build script): (`OUT_DIR`, name, identity).
static BUILT: Mutex<Vec<(PathBuf, String, String)>> = Mutex::new(Vec::new());

/// Makes sure no two configurations share a model prefix.
///
/// Why: two models with the same `--prefix` define the same C++ symbols, and a static
/// link silently keeps one copy (88 of 88 strong symbols collide for the counter;
/// docs/notes/spikes.md), so one model would run the other's code. Two checks:
/// - in this build script, a DUT name may be built again only with the same
///   configuration (the name also names `OUT_DIR/simjig/<name>.rs`);
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
    {
        let mut built = BUILT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let same_name = built
            .iter()
            .find(|(out, name, _)| *out == env.out_dir && *name == config.name);
        match same_name {
            Some((_, _, id)) if *id != identity => {
                return fail(
                    "a DUT of this name was already built with a different configuration in \
                     this build script; give each configuration its own name"
                        .to_owned(),
                );
            }
            Some(_) => {}
            None => built.push((env.out_dir.clone(), config.name.clone(), identity.clone())),
        }
    }
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
pub(crate) fn design_ir(
    config: &DutConfig,
    top: &str,
    verilator: &Verilator,
    cache: &Cache,
) -> Result<Ir, Error> {
    let Some(iface) = config.iface_top() else {
        return Ok(Ir {
            design: json_pass(config, top, &[], verilator)?,
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
    let elab_ir = json_pass(config, &elab_top, &[elab], verilator)?;
    let plan = codegen::lower_wrapper(&elab_ir, &iface).map_err(codegen_err)?;
    let wrapper_top = codegen::wrapper_module(top);
    let wrapper = cache.store_input(&GeneratedInput {
        file_name: format!("{wrapper_top}.sv"),
        contents: codegen::emit_wrapper(&plan),
    })?;
    let design = json_pass(
        config,
        &wrapper_top,
        std::slice::from_ref(&wrapper),
        verilator,
    )?;
    Ok(Ir {
        design,
        wrapper: Some(wrapper),
        top: wrapper_top,
    })
}

/// Runs one JSON pass with the located Verilator, the one that builds the model: the IR
/// and the model must come from the same elaboration rules.
fn json_pass(
    config: &DutConfig,
    top: &str,
    extra: &[PathBuf],
    verilator: &Verilator,
) -> Result<Design, Error> {
    svtypes::verilator::json_pass(&config.json_pass_opts(top, extra, verilator))
        .map_err(|e| Error::json_pass(&config.name, e))
}

/// Lints the layout check with the model's flags, without `-G` (docs/notes/codegen.md).
/// It runs on every build (it is not cached): a `--lint-only` run, small next to the
/// C++ compile.
/// A layout mismatch is an elaboration-time `$fatal`, which 5.052 reports as the
/// `USERFATAL` warning; [`codegen::LAYOUT_CHECK_FLAGS`] makes it an error.
pub(crate) fn layout_check(
    config: &DutConfig,
    verilator: &Verilator,
    cache: &Cache,
    check: &SvModule,
) -> Result<(), Error> {
    let file = cache.store_input(&GeneratedInput {
        file_name: format!("{}.sv", check.module),
        contents: check.contents.clone(),
    })?;
    let mut cmd = verilator.command();
    cmd.args(layout_check_args(config, &check.module, &file));
    verilator::run(&mut cmd).map(|_| ())
}

fn layout_check_args(config: &DutConfig, module: &str, file: &Path) -> Vec<String> {
    // `-Wno-fatal` first: lint policy belongs to the model build, which sees the same
    // sources. Without it, a design with `timeunit` fails here on TIMESCALEMOD, because
    // the generated module declares none. `-Werror-USERFATAL` still makes a layout
    // mismatch fatal.
    let mut a = vec!["-Wno-fatal".to_owned()];
    a.extend(codegen::LAYOUT_CHECK_FLAGS.iter().map(|f| (*f).to_owned()));
    a.push(
        if config.timing {
            "--timing"
        } else {
            "--no-timing"
        }
        .to_owned(),
    );
    let sources = config
        .sources
        .iter()
        .cloned()
        .chain([file.to_path_buf()])
        .collect();
    a.extend(config.design(module, sources, false).design_args());
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
        let args = layout_check_args(&config, "simjig_layout_c", Path::new("/g/l.sv"));
        assert_eq!(
            args.join(" "),
            "-Wno-fatal --lint-only -Werror-USERFATAL --no-timing --top-module simjig_layout_c -DK=V \
             /r/c.sv /g/l.sv"
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
