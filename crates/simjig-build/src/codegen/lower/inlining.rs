//! Probes require a fully inlined model.
//!
//! A probe is reached as a member of the root class, `rootp-><top>__DOT__<inst>__DOT__<var>`
//! ([`SignalTarget::Probe`](super::super::SignalTarget::Probe)). Verilator 5.052 names
//! members that way only for modules inlined into the root. A module kept apart becomes its
//! own class reached through `vlSymsp` (`TOP__<top>__<inst>`), with plain member names
//! (`sum`), and the shim then fails to compile. That happens with:
//! - `-fno-inline`, or `-O0` (which turns every optimisation off), unless a later
//!   `-finline`/`-O1..3` turns inlining back on (Verilator options are last-wins);
//! - `--hierarchical` (each hierarchy block is a separate model);
//! - a control-file `no_inline -module` or `hier_block -module` line.
//!
//! These are rejected here with the reason. The `/*verilator no_inline_module*/` and
//! `/*verilator hier_block*/` metacomments live in the sources, which the pure lowering
//! does not read; neither are `-f` option files or `.vlt` files given as plain sources.
//! The general fix is to find probes through `VerilatedScope::varFind` at create time
//! (docs/notes/s1fix-codegen.md).

use super::super::{CodegenConfig, Error};

/// Fails when `config` has probes and a setting that keeps a module from being inlined.
pub(super) fn check(config: &CodegenConfig) -> Result<(), Error> {
    let Some(first) = config.probes.first() else {
        return Ok(());
    };
    let Some(cause) = flag_cause(&config.verilator_args).or_else(|| vlt_cause(config)) else {
        return Ok(());
    };
    Err(Error::unsupported(
        format!("probe {}", first.path),
        format!(
            "probes are reached as members of the model root (`top__DOT__u__DOT__x`), which \
             exist only when every module is inlined, but {cause} keeps modules apart; drop \
             it, or drop the probes"
        ),
    ))
}

/// The last argument that leaves inlining off or the build hierarchical, if any.
fn flag_cause(args: &[String]) -> Option<String> {
    let (mut no_inline, mut hierarchical) = (None, None);
    for arg in args {
        // Verilator accepts every option with one dash or two.
        match arg.trim_start_matches('-') {
            "fno-inline" | "O0" => no_inline = Some(arg),
            "finline" | "O1" | "O2" | "O3" => no_inline = None,
            "hierarchical" => hierarchical = Some(arg),
            "no-hierarchical" => hierarchical = None,
            _ => {}
        }
    }
    no_inline
        .or(hierarchical)
        .map(|a| format!("the Verilator argument `{a}`"))
}

/// The first control-file line that keeps a module apart, if any.
fn vlt_cause(config: &CodegenConfig) -> Option<String> {
    config
        .control_files
        .iter()
        .flat_map(|text| text.lines())
        .map(|line| line.split("//").next().unwrap_or("").trim())
        .find(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let module_scoped = words.contains(&"-module")
                && !words.contains(&"-task")
                && !words.contains(&"-function");
            module_scoped && matches!(words.first(), Some(&("no_inline" | "hier_block")))
        })
        .map(|line| format!("the control-file line `{line}`"))
}
