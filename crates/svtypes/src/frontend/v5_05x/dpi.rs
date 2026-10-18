//! DPI-C imports and exports in the 5.05x dialect.
//!
//! Facts about the dialect (checked against 5.052 output, `tests/fixtures/dpi`):
//! - An import is a `FUNC` or `TASK` with `dpiImport`, directly in a module's or package's
//!   `stmtsp`; an export is one with `dpiExport`. `cname` is the C symbol (the alias of
//!   `import "DPI-C" c = function ...`).
//! - A `void` function is a `TASK` too, and neither `dpiTask` nor `dpiContext` nor `pure` is
//!   dumped, so a task and a `void` function look the same.
//! - A function's result is the single `VAR` in `fvarp`; the formal arguments are the
//!   `VAR`s with a direction in `stmtsp`, in declaration order. `stmtsp` also holds an
//!   `INITIALAUTOMATICSTMT` zeroing the outputs, which carries no signature information.
//! - A parameterised module is cloned per parameter set, so one import can appear several
//!   times; the copies must agree.
//!
//! Types are classified only as far as they decide the C ABI (IEEE 1800-2023 Annex H);
//! anything outside the basic integral types becomes [`DpiType::Unsupported`], so a
//! design whose DPI the Rust back end cannot implement still decodes.

use std::collections::{BTreeMap, BTreeSet};

use super::super::json::{Index, Node};
use super::{Decoder, VAR_FIELDS};
use crate::error::Result;
use crate::ir::{DpiArg, DpiImport, DpiType};

/// Fields of a DPI `FUNC`/`TASK` node in this dialect.
const FTASK_FIELDS: &[&str] = &[
    "dpiImport",
    "dpiExport",
    "prototype",
    "cname",
    "fvarp",
    "stmtsp",
    "classOrPackagep",
    "scopeNamep",
    "lifetime",
    "ifacePortName",
];

/// Fields of an argument or result `VAR`, beyond those of a port.
const DPI_VAR_FIELDS: &[&str] = &["isFuncLocal", "isFuncReturn", "noCReset"];

/// The imports (sorted and deduplicated by C name) and export C names (sorted) declared
/// directly in `scopes`.
pub(super) fn collect<'a>(
    d: &Decoder<'a>,
    scopes: &[Node<'a>],
) -> Result<(Vec<DpiImport>, Vec<String>)> {
    let ix = &d.ix;
    let mut imports: BTreeMap<String, DpiImport> = BTreeMap::new();
    let mut exports = BTreeSet::new();
    for scope in scopes {
        for stmt in ix.children(scope, "stmtsp")? {
            if !matches!(Index::kind(&stmt), "FUNC" | "TASK") {
                continue;
            }
            if ix.flag(&stmt, "dpiExport")? {
                exports.insert(ix.str(&stmt, "cname")?.to_owned());
            }
            if !ix.flag(&stmt, "dpiImport")? {
                continue;
            }
            let import = decode_import(d, &stmt)?;
            match imports.get(&import.c_name) {
                Some(seen) if !same_signature(seen, &import) => {
                    return Err(ix.err(
                        &stmt,
                        format!(
                            "DPI-C import `{}` is declared twice with different signatures; \
                             one C symbol has one prototype",
                            import.c_name
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    imports.insert(import.c_name.clone(), import);
                }
            }
        }
    }
    Ok((
        imports.into_values().collect(),
        exports.into_iter().collect(),
    ))
}

/// Whether two declarations of one C name have the same C prototype.
fn same_signature(a: &DpiImport, b: &DpiImport) -> bool {
    let shape = |i: &DpiImport| {
        i.args
            .iter()
            .map(|a| (a.dir, a.ty.clone()))
            .collect::<Vec<_>>()
    };
    a.result == b.result && shape(a) == shape(b)
}

fn decode_import<'a>(d: &Decoder<'a>, node: &Node<'a>) -> Result<DpiImport> {
    let ix = &d.ix;
    ix.check_fields(node, FTASK_FIELDS)?;
    let result = if Index::kind(node) == "FUNC" {
        let var = ix.only_child(node, "fvarp")?;
        ix.check_fields(&var, &[VAR_FIELDS, DPI_VAR_FIELDS].concat())?;
        Some(dpi_type(d, &ix.deref(&var, "dtypep")?)?)
    } else {
        None
    };
    let mut args = Vec::new();
    for stmt in ix.children(node, "stmtsp")? {
        match Index::kind(&stmt) {
            "VAR" => {
                ix.check_fields(&stmt, &[VAR_FIELDS, DPI_VAR_FIELDS].concat())?;
                args.push(DpiArg {
                    name: ix.str(&stmt, "name")?.to_owned(),
                    dir: d.direction(&stmt)?,
                    ty: dpi_type(d, &ix.deref(&stmt, "dtypep")?)?,
                });
            }
            "INITIALAUTOMATICSTMT" => {}
            other => {
                return Err(ix.err(
                    &stmt,
                    format!("unexpected {other} in the declaration of a DPI-C import"),
                ));
            }
        }
    }
    Ok(DpiImport {
        name: ix.str(node, "name")?.to_owned(),
        c_name: ix.str(node, "cname")?.to_owned(),
        result,
        args,
        loc: ix.source_loc(node).map(|loc| file_line(&loc)),
    })
}

/// `dpi.sv:13` from `/abs/path/dpi.sv:13:41`: the file name stays stable across checkouts,
/// so the IR (and every hash over it) does too.
fn file_line(loc: &str) -> String {
    let base = loc.rsplit('/').next().unwrap_or(loc);
    let mut parts = base.splitn(3, ':');
    match (parts.next(), parts.next()) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        _ => base.to_owned(),
    }
}

/// The DPI classification of a dtype node.
fn dpi_type<'a>(d: &Decoder<'a>, node: &Node<'a>) -> Result<DpiType> {
    let ix = &d.ix;
    let typedef = (Index::kind(node) == "REFDTYPE")
        .then(|| ix.str(node, "name"))
        .transpose()?;
    let resolved = d.resolve(node)?;
    let named = |what: &str| match typedef {
        Some(name) => format!("{what} `{name}`"),
        None => what.to_owned(),
    };
    let ty = match Index::kind(&resolved) {
        "BASICDTYPE" => basic(ix, &resolved)?,
        "ENUMDTYPE" => DpiType::Unsupported(named("an enum")),
        "STRUCTDTYPE" => DpiType::Unsupported(named("a struct")),
        "UNIONDTYPE" => DpiType::Unsupported(named("a union")),
        "PACKARRAYDTYPE" => DpiType::Unsupported(named("a packed array")),
        "UNPACKARRAYDTYPE" | "UNSIZEDARRAYDTYPE" => DpiType::Unsupported(named("an array")),
        other => DpiType::Unsupported(named(&format!("a {other} type"))),
    };
    Ok(ty)
}

fn basic<'a>(ix: &Index<'a>, node: &Node<'a>) -> Result<DpiType> {
    ix.check_fields(node, &["keyword", "range", "generic", "signed"])?;
    let signed = ix.flag(node, "signed")?;
    let width = match ix.opt_str(node, "range")? {
        Some(text) => Some(
            text.parse::<crate::ir::Range>()
                .and_then(crate::ir::Range::count)
                .map_err(|e| ix.err(node, e.0))?,
        ),
        None => None,
    };
    let keyword = ix.str(node, "keyword")?;
    Ok(match (keyword, width) {
        ("bit", None) => DpiType::Bit,
        ("logic", None) => DpiType::Logic,
        ("bit", Some(width)) => DpiType::BitVector { width, signed },
        ("logic", Some(width)) => DpiType::LogicVector { width, signed },
        ("byte", _) => DpiType::Byte { signed },
        ("shortint", _) => DpiType::ShortInt { signed },
        ("int", _) => DpiType::Int { signed },
        ("longint", _) => DpiType::LongInt { signed },
        ("chandle", _) => DpiType::Chandle,
        (other, _) => DpiType::Unsupported(format!("`{other}`")),
    })
}

#[cfg(test)]
mod tests {
    use super::file_line;

    #[test]
    fn locations_keep_the_file_name_and_line() {
        assert_eq!(file_line("/a/b/dpi.sv:13:41"), "dpi.sv:13");
        assert_eq!(file_line("dpi.sv:2:3"), "dpi.sv:2");
        assert_eq!(file_line("odd"), "odd");
    }
}
