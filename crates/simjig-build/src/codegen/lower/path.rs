//! The probe path check: a probe's hierarchical path (`top.g_x.u_core.state`) walked
//! through the IR's hierarchy before verilating, so a mistyped instance, generate block or
//! variable, a wrong declaring module or a writable constant fails the build with the fix.
//!
//! A path segment is an instance or a generate block, named as Verilator names its scope:
//! the label, `genblk<N>` for an unnamed block (IEEE 1800-2023 §27.6) or `label[i]` for one
//! iteration of a generate-for loop (§27.4). A generate block stays in its module: the
//! probe's module is the module of the last instance on the path. Instance arrays are
//! refused (their scopes, `u[0]`, are not modelled). The width is checked after
//! verilation, against Verilator's scope table (the IR has no types for internal
//! variables).

use svtypes::ir::{Design, GenerateBlock, Instance, Module, Variable, VariableKind};

use super::super::names::closest;
use super::super::{Probe, ProbeAccess, Wrapper};
use super::DUT_INSTANCE;

/// One level of the walk: a module body, or a generate block inside one.
struct Level<'a> {
    /// The module whose body this is (for a generate block, the enclosing module).
    module: &'a Module,
    /// Set for a generate block.
    in_block: bool,
    instances: &'a [Instance],
    variables: &'a [Variable],
    blocks: &'a [GenerateBlock],
}

impl<'a> Level<'a> {
    fn module(module: &'a Module) -> Self {
        Self {
            module,
            in_block: false,
            instances: &module.instances,
            variables: &module.variables,
            blocks: &module.generate_blocks,
        }
    }

    fn block(module: &'a Module, b: &'a GenerateBlock) -> Self {
        Self {
            module,
            in_block: true,
            instances: &b.instances,
            variables: &b.variables,
            blocks: &b.generate_blocks,
        }
    }

    fn instance(&self, name: &str) -> Option<&'a Instance> {
        self.instances.iter().find(|i| i.name == name)
    }

    fn block_named(&self, name: &str) -> Option<&'a GenerateBlock> {
        self.blocks.iter().find(|b| b.name == name)
    }

    /// The first iteration of the generate-for loop `label`, if there is one here.
    fn loop_iteration(&self, label: &str) -> Option<&'a GenerateBlock> {
        self.blocks.iter().find(|b| {
            b.name
                .strip_prefix(label)
                .is_some_and(|rest| rest.starts_with('['))
        })
    }

    /// The names a segment could have meant here.
    fn scope_names(&self) -> impl Iterator<Item = &'a str> {
        let instances = self.instances.iter().map(|i| i.name.as_str());
        instances.chain(self.blocks.iter().map(|b| b.name.as_str()))
    }
}

/// `"; did you mean `x`?"` for the closest candidate, or nothing.
fn hint<'c>(name: &str, candidates: impl IntoIterator<Item = &'c str>) -> String {
    closest(name, candidates)
        .map(|c| format!("; did you mean `{c}`?"))
        .unwrap_or_default()
}

/// A path segment: an identifier, or an identifier with one index (`g[0]`, a generate-for
/// iteration). `[A-Za-z_][A-Za-z0-9_]*`, optionally followed by `[<digits>]`.
fn is_path_segment(s: &str) -> bool {
    match s.strip_suffix(']').and_then(|s| s.split_once('[')) {
        Some((ident, index)) => {
            is_simple_ident(ident) && !index.is_empty() && index.chars().all(|c| c.is_ascii_digit())
        }
        None => is_simple_ident(s),
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
fn is_simple_ident(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Checks `segments` (the probe path split at `.`): the start, the spelling of each
/// segment, the probe's module, then the walk and the variable at the end.
pub(super) fn check_probe_path(
    design: &Design,
    probe: &Probe,
    segments: &[&str],
    wrapper: Option<&Wrapper>,
) -> Result<(), String> {
    check_spelling(design, probe, segments, wrapper)?;
    let mut module: &Module = design.top_module();
    if wrapper.is_some() {
        let dut = module.instances.iter().find(|i| i.name == DUT_INSTANCE);
        module = dut.map_or(module, |i| design.module(i.module));
    }
    let mut level = Level::module(module);
    let (var, scopes) = segments[1..].split_last().unwrap_or((&"", &[]));
    for (depth, name) in scopes.iter().enumerate() {
        let here = segments[..=depth].join(".");
        level = step(design, &level, name, &here)?;
    }
    let scope = segments[..segments.len() - 1].join(".");
    check_variable(probe, &level, var, &scope)
}

/// What can be checked without walking: the path starts with the DUT, its segments are
/// path segments and its variable a plain identifier, and the probe's module exists.
fn check_spelling(
    design: &Design,
    probe: &Probe,
    segments: &[&str],
    wrapper: Option<&Wrapper>,
) -> Result<(), String> {
    let (what, dut) = match wrapper {
        Some(w) => ("the DUT", w.dut.as_str()),
        None => ("the top module", design.top_module().orig_name.as_str()),
    };
    if segments.len() < 2 || segments[0] != dut {
        return Err(format!(
            "the path must start with {what} `{dut}`, e.g. `{dut}.u_core.state`"
        ));
    }
    let (var, scopes) = segments.split_last().unwrap_or((&"", &[]));
    if let Some(bad) = scopes.iter().find(|s| !is_path_segment(s)) {
        return Err(format!(
            "`{bad}` is not a plain identifier or a generate-for iteration (`g[0]`)"
        ));
    }
    if !is_simple_ident(var) {
        return Err(format!("`{var}` is not a plain identifier"));
    }
    if !design.modules.iter().any(|m| m.orig_name == probe.module) {
        let names = design.modules.iter().map(|m| m.orig_name.as_str());
        return Err(format!(
            "no module `{}` in the design{}",
            probe.module,
            hint(&probe.module, names)
        ));
    }
    Ok(())
}

/// One step down from `level` into the instance or generate block `name`; `here` is the
/// path of `level`.
fn step<'a>(
    design: &'a Design,
    level: &Level<'a>,
    name: &str,
    here: &str,
) -> Result<Level<'a>, String> {
    let base = name.split_once('[').map_or(name, |(b, _)| b);
    if let Some(inst) = level.instance(base).filter(|i| i.array) {
        return Err(format!(
            "`{}` is an instance array; instance arrays are not supported in probe paths",
            inst.name
        ));
    }
    if let Some(inst) = level.instance(name) {
        return Ok(Level::module(design.module(inst.module)));
    }
    if let Some(b) = level.block_named(name) {
        return Ok(Level::block(level.module, b));
    }
    if let Some(first) = level.loop_iteration(name) {
        return Err(format!(
            "`{name}` is a generate-for loop; name one iteration's scope as Verilator \
             elaborates it, e.g. `{here}.{}`",
            first.name
        ));
    }
    let what = if level.blocks.is_empty() {
        "instance"
    } else {
        "instance or generate block"
    };
    Err(format!(
        "no {what} `{name}` in `{here}` (a `{}`){}",
        level.module.orig_name,
        hint(name, level.scope_names())
    ))
}

/// Checks the probe's module and its variable `var` at the end of the path (`scope`).
fn check_variable(probe: &Probe, level: &Level<'_>, var: &str, scope: &str) -> Result<(), String> {
    let module = level.module;
    let found = level.variables.iter().find(|v| v.name == var);
    if found.is_none()
        && let Some(why) = names_a_scope(level, var, scope)
    {
        return Err(why);
    }
    if module.orig_name != probe.module {
        return Err(format!(
            "`{scope}` is an instance of `{}`, not of `{}` as the probe declares; pass `\"{}\"` \
             as the probe's module",
            module.orig_name, probe.module, module.orig_name
        ));
    }
    let Some(v) = found else {
        return Err(missing_variable(level, var, scope));
    };
    let constant = match v.kind {
        VariableKind::Signal => None,
        VariableKind::Parameter => Some("parameter"),
        VariableKind::Localparam => Some("localparam"),
        _ => return Err(format!("`{var}`: {}", super::super::error::UNKNOWN_IR_KIND)),
    };
    if let (Some(what), ProbeAccess::ReadWrite) = (constant, probe.access) {
        return Err(format!(
            "`{var}` is a {what} of `{}`, a constant Verilator keeps in read-only storage; \
             probe it with `ProbeAccess::Read`",
            module.orig_name
        ));
    }
    Ok(())
}

/// When the path stops one level short, at an instance or generate block `var`: why.
fn names_a_scope(level: &Level<'_>, var: &str, scope: &str) -> Option<String> {
    let kind = if level.instance(var).is_some() {
        "an instance"
    } else if level.block_named(var).is_some() || level.loop_iteration(var).is_some() {
        "a generate block"
    } else {
        return None;
    };
    Some(format!(
        "`{scope}.{var}` is {kind}, not a variable; a probe path ends at a variable, e.g. \
         `{scope}.{var}.<variable>`"
    ))
}

/// Why `var` is not a variable of `level`, with the closest variable name.
fn missing_variable(level: &Level<'_>, var: &str, scope: &str) -> String {
    let names = level.variables.iter().map(|v| v.name.as_str());
    let module = &level.module.orig_name;
    if level.in_block {
        format!(
            "no variable `{var}` in the generate block `{scope}` of `{module}`{}",
            hint(var, names)
        )
    } else {
        format!(
            "no variable `{var}` in `{module}` (the module of `{scope}`){}",
            hint(var, names)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_segments_are_identifiers_or_loop_iterations() {
        assert!(is_simple_ident("_x1") && !is_simple_ident("1x") && !is_simple_ident("a$b"));
        assert!(is_path_segment("g[0]") && is_path_segment("g_x") && is_path_segment("g[12]"));
        for bad in ["g[]", "g[i]", "[0]", "g[0][1]", "g[-1]", "g[0"] {
            assert!(!is_path_segment(bad), "{bad}");
        }
    }
}
