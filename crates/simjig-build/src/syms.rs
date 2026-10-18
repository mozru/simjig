//! The build-time probe check against Verilator's scope table.
//!
//! `simjig::find_probe` (`cpp/simjig_rt.cpp`) binds each probe at create by looking it up in
//! the scope table that `V<prefix>__Syms__Slow.cpp` builds. The same table is plain C++
//! text, so the build reads it right after verilating and fails there, with the probe's
//! name, instead of at the first `create`:
//! - a scope or variable the table lacks (a probe inside a hierarchical block, or a
//!   `.vlt` that did not make it public);
//! - a width other than the probe's;
//! - an unpacked variable, or one that is not stored as a packed vector (a `real`, a
//!   `string`: its `VLVT_*` type), or whose storage is not the one its width has;
//! - a writable probe of a parameter, or of a variable that is not `public_flat_rw`.
//!
//! Path, instance and module mistakes are caught earlier, by the lowering against the IR
//! (`codegen::lower`), which knows every instance and variable; this table only lists
//! what is public. The create-time check stays as the backstop.
//!
//! Facts about the table (Verilator 5.052, `V3EmitCSyms.cpp`; checked on its output):
//! - `VlScopeTableEntry` rows are `{offsetof(<Syms>, <member>), "<scope>", ...}`, the scope
//!   named without the model's instance name (`alu.u_core`);
//! - `VlVarTableEntry` rows are `{"<var>", offsetof(..), VLVT_<type>, (<flags>), <udims>,
//!   <pdims>, {<left>, <right>, ...}}`, the unpacked ranges before the packed ones; a
//!   scalar has `pdims` 0;
//! - `<member>->varsInsertFromTable(<table>, ...)` gives a scope its table, and
//!   `<member>->varInsert("<var>", <ptr>, <isParam>, VLVT_<type>, <flags>, <udims>,
//!   <pdims> ,<left>,<right>...)` adds one variable (parameters are added this way).

use std::collections::BTreeMap;
use std::path::Path;

use svtypes::ir::Storage;
use svtypes::width::packed_storage;

use crate::Error;
use crate::codegen::closest;
use crate::emit::ProbeSite;

/// The packed storage a `VerilatedVarType` (`verilated.h`) names: `VLVT_UINT8/16/32/64`
/// and `VLVT_WDATA` (whose word count the table does not give; [`storage_holds`]
/// compares kinds only). `None` for the others (`VLVT_REAL`, `VLVT_STRING`, ...).
fn packed_kind(vltype: &str) -> Option<Storage> {
    Some(match vltype {
        "VLVT_UINT8" => Storage::CData,
        "VLVT_UINT16" => Storage::SData,
        "VLVT_UINT32" => Storage::IData,
        "VLVT_UINT64" => Storage::QData,
        "VLVT_WDATA" => Storage::VlWide { words: 0 },
        _ => return None,
    })
}

/// Whether Verilator's storage `kind` is the one simjig binds for `width` bits
/// (`svtypes::width::packed_storage`, as `simjig::find_probe` checks at create).
fn storage_holds(kind: &Storage, width: u32) -> bool {
    packed_storage(width)
        .is_ok_and(|want| std::mem::discriminant(&want) == std::mem::discriminant(kind))
}

/// The SystemVerilog type Verilator stores as `vltype`, for messages.
fn sv_type(vltype: &str) -> &str {
    match vltype {
        "VLVT_REAL" => "real",
        "VLVT_STRING" => "string",
        "VLVT_STRUCT" => "unpacked struct",
        "VLVT_UNION" => "unpacked union",
        "VLVT_PTR" => "pointer",
        other => other,
    }
}

/// One public variable of a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Var {
    name: String,
    /// The `VLVT_*` enumerator Verilator stores it as.
    vltype: String,
    /// Packed bits of one element; `None` when the ranges could not be read.
    width: Option<u32>,
    unpacked: bool,
    param: bool,
    writable: bool,
}

/// The public scopes of a model, by scope name.
#[derive(Debug, Default)]
struct Scopes(BTreeMap<String, Vec<Var>>);

/// Reads the scope table of the model `prefix` in `obj_dir` (every `<prefix>__Syms*.cpp`).
fn read(obj_dir: &Path, prefix: &str) -> Result<Scopes, Error> {
    let mut text = String::new();
    let entries = std::fs::read_dir(obj_dir).map_err(Error::io("listing", obj_dir))?;
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                // Verilator writes the extension in lower case.
                n.starts_with(&format!("{prefix}__Syms"))
                    && Path::new(n).extension().is_some_and(|e| e == "cpp")
            })
        })
        .collect();
    files.sort();
    for f in &files {
        text.push_str(&std::fs::read_to_string(f).map_err(Error::io("reading", f))?);
        text.push('\n');
    }
    Ok(parse(&text))
}

/// A call that adds variables to a scope, by the scope's `Syms` member.
enum Insert<'t> {
    /// `varsInsertFromTable(<table>, ..)`.
    Table(&'t str),
    /// `varInsert(..)`.
    Var(Var),
}

/// The scope table in the text of the `__Syms` sources (see the module docs).
///
/// With `--output-split`, the tables, the scope rows and the insert calls are in separate
/// files, so every line is read before a call is resolved: the result does not depend on
/// the order of the files.
fn parse(text: &str) -> Scopes {
    let mut members: BTreeMap<&str, String> = BTreeMap::new();
    let mut tables: BTreeMap<&str, Vec<Var>> = BTreeMap::new();
    let mut inserts: Vec<(&str, Insert<'_>)> = Vec::new();
    let mut table: Option<&str> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("extern const VlVarTableEntry ") {
            // A definition (`<name>[] = {`), not a forward declaration (`<name>[];`).
            table = rest
                .strip_suffix("= {")
                .and_then(|r| r.split_once("[]"))
                .map(|(name, _)| name);
            continue;
        }
        if line.starts_with("};") {
            table = None;
            continue;
        }
        if let Some(t) = table {
            if let Some(v) = table_row(line) {
                tables.entry(t).or_default().push(v);
            }
        } else if let Some(rest) = line.strip_prefix("{offsetof(") {
            // `<Syms>, <member>), "<scope>", ...`
            let member = rest.split_once(')').and_then(|(o, _)| o.split_once(','));
            let fields = split_top(rest.split_once(')').map_or("", |(_, r)| r));
            if let (Some((_, member)), Some(name)) = (member, fields.get(1)) {
                members.insert(member.trim(), unquote(name).to_owned());
            }
        } else if let Some((member, call)) = line.split_once("->") {
            if let Some(args) = call.strip_prefix("varsInsertFromTable(") {
                let t = args.split(',').next().unwrap_or("").trim();
                inserts.push((member, Insert::Table(t)));
            } else if let Some(args) = call.strip_prefix("varInsert(")
                && let Some(v) = insert_call(args)
            {
                inserts.push((member, Insert::Var(v)));
            }
        }
    }
    let mut scopes = Scopes::default();
    for (member, insert) in inserts {
        let scope = members.get(member).cloned().unwrap_or_default();
        let vars = scopes.0.entry(scope).or_default();
        match insert {
            Insert::Table(t) => vars.extend(tables.get(t).cloned().unwrap_or_default()),
            Insert::Var(v) => vars.push(v),
        }
    }
    scopes
}

/// `{"r", offsetof(..), VLVT_UINT8, (VLVD_NODIR|VLVF_PUB_RD), 0, 1, {7, 0, 0, 0, 0, 0}},`
fn table_row(line: &str) -> Option<Var> {
    let f = split_top(line.strip_prefix('{')?);
    let name = unquote(f.first()?);
    var(
        name,
        false,
        f.get(2)?,
        f.get(3)?,
        f.get(4)?,
        f.get(5)?,
        f.get(6).copied().unwrap_or(""),
    )
}

/// `"LP", <ptr>, true, VLVT_UINT8, VLVD_NODIR|VLVF_PUB_RW, 0, 1 ,7,0);`
fn insert_call(args: &str) -> Option<Var> {
    let f = split_top(args.trim_end().trim_end_matches(';').trim_end_matches(')'));
    let ranges = f.get(7..).map(|r| r.join(",")).unwrap_or_default();
    var(
        unquote(f.first()?),
        f.get(2)? == &"true",
        f.get(3)?,
        f.get(4)?,
        f.get(5)?,
        f.get(6)?,
        &ranges,
    )
}

fn var(
    name: &str,
    param: bool,
    vltype: &str,
    flags: &str,
    udims: &str,
    pdims: &str,
    ranges: &str,
) -> Option<Var> {
    let udims: usize = udims.trim().parse().ok()?;
    let pdims: usize = pdims.trim().parse().ok()?;
    let bounds: Vec<i64> = ranges
        .split(|c: char| !(c.is_ascii_digit() || c == '-'))
        .filter_map(|n| n.parse().ok())
        .collect();
    let width = (0..pdims).try_fold(1u32, |w, d| {
        let at = 2 * (udims + d);
        let (l, r) = (bounds.get(at)?, bounds.get(at + 1)?);
        w.checked_mul(u32::try_from(l.abs_diff(*r) + 1).ok()?)
    });
    Some(Var {
        name: name.to_owned(),
        vltype: vltype.trim().to_owned(),
        width,
        unpacked: udims > 0,
        param,
        writable: flags.contains("VLVF_PUB_RW"),
    })
}

/// Splits at the commas that are not inside parentheses, braces or quotes.
fn split_top(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut quoted, mut start) = (Vec::new(), 0i32, false, 0);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' | '{' if !quoted => depth += 1,
            ')' | '}' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"')
}

/// Checks every probe the shim binds against the model's scope table.
pub(crate) fn check_probes(
    dut: &str,
    obj_dir: &Path,
    prefix: &str,
    probes: &[ProbeSite],
) -> Result<(), Error> {
    if probes.is_empty() {
        return Ok(());
    }
    let scopes = read(obj_dir, prefix)?;
    for p in probes {
        if let Err(why) = check(&scopes, p) {
            return Err(Error::Config {
                dut: dut.to_owned(),
                msg: format!("probe `{}` (declared in build.rs): {why}", p.path),
            });
        }
    }
    Ok(())
}

fn check(scopes: &Scopes, p: &ProbeSite) -> Result<(), String> {
    let Some(vars) = scopes.0.get(&p.scope) else {
        return Err(format!(
            "Verilator made no variable of `{}` public, so the model has no scope `{}` to \
             find it in (a probe inside a hierarchical block, `hier_block`, is in another \
             model)",
            p.path, p.scope
        ));
    };
    let Some(v) = vars.iter().find(|v| v.name == p.var) else {
        let hint = closest(&p.var, vars.iter().map(|v| v.name.as_str()))
            .map(|c| format!("; did you mean `{c}`?"))
            .unwrap_or_default();
        return Err(format!(
            "Verilator did not make `{}` of `{}` public{hint}",
            p.var, p.module
        ));
    };
    if v.unpacked {
        return Err(format!(
            "`{}` is an unpacked array; only packed variables can be probed",
            p.var
        ));
    }
    let Some(storage) = packed_kind(&v.vltype) else {
        return Err(format!(
            "`{}` is a `{}` (Verilator stores it as {}); only packed variables (bit vectors, \
             packed structs and unions, enums) can be probed",
            p.var,
            sv_type(&v.vltype),
            v.vltype
        ));
    };
    if let Some(width) = v.width
        && !storage_holds(&storage, width)
    {
        return Err(format!(
            "`{}` is {width} bits wide, but Verilator stores it as {}, which is not the \
             storage of that width",
            p.var, v.vltype
        ));
    }
    if let Some(width) = v.width
        && width != p.width
    {
        return Err(format!(
            "`{}` is {width} bits wide in the design, but the probe's type is {} bits; declare \
             it as `ProbeType::Bits {{ width: {width} }}` or a type of that width",
            p.var, p.width
        ));
    }
    if p.writable && v.param {
        return Err(format!(
            "`{}` is a parameter (constant storage); probe it with `ProbeAccess::Read`",
            p.var
        ));
    }
    if p.writable && !v.writable {
        return Err(format!(
            "`{}` is not public_flat_rw, but the probe is read-write; a `.vlt` among the \
             inputs may have made it public_flat_rd",
            p.var
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of 5.052's output: a root table, a parameter added by `varInsert`, and a
    /// scope whose variables live in a non-inlined module's table.
    const SYMS: &str = r#"
extern const VlVarTableEntry Vx___024root__VpiVarTable0[] = {
    {"r", offsetof(Vx___024root, x__DOT__r), VLVT_UINT8, (VLVD_NODIR|VLVF_PUB_RD), 0, 1, {7, 0, 0, 0, 0, 0}},
    {"one", offsetof(Vx___024root, x__DOT__one), VLVT_UINT8, (VLVD_NODIR|VLVF_PUB_RW), 0, 0, {0, 0, 0, 0, 0, 0}},
    {"two", offsetof(Vx___024root, x__DOT__two), VLVT_UINT32, (VLVD_NODIR|VLVF_PUB_RW), 0, 2, {3, 0, 7, 0, 0, 0}},
    {"un", offsetof(Vx___024root, x__DOT__un), VLVT_UINT8, (VLVD_NODIR|VLVF_PUB_RW), 1, 1, {0, 3, 7, 0, 0, 0}},
    {"re", offsetof(Vx___024root, x__DOT__re), VLVT_REAL, (VLVD_NODIR|VLVF_PUB_RD), 0, 0, {0, 0, 0, 0, 0, 0}},
    {"st", offsetof(Vx___024root, x__DOT__st), VLVT_STRING, (VLVD_NODIR|VLVF_PUB_RD), 0, 0, {0, 0, 0, 0, 0, 0}},
    {"odd", offsetof(Vx___024root, x__DOT__odd), VLVT_UINT16, (VLVD_NODIR|VLVF_PUB_RD), 0, 1, {7, 0, 0, 0, 0, 0}},
};
extern const VlVarTableEntry Vx_leaf__VpiVarTable1[] = {
    {"s", offsetof(Vx_leaf, s), VLVT_UINT16, (VLVD_NODIR|VLVF_PUB_RW), 0, 1, {0, 9, 0, 0, 0, 0}},
};
extern const VlScopeTableEntry Vx__Syms__VpiScopeTable[] = {
    {offsetof(Vx__Syms, __Vscopep_x), "x", "x", "<null>", 0, VerilatedScope::SCOPE_OTHER},
    {offsetof(Vx__Syms, __Vscopep_x__u), "x.u", "u", "<null>", 0, VerilatedScope::SCOPE_OTHER},
};
    __Vscopep_x->varsInsertFromTable(Vx___024root__VpiVarTable0, 7, &(TOP));
    __Vscopep_x->varInsert("LP", const_cast<void*>(static_cast<const void*>(&(TOP.x__DOT__LP))), true, VLVT_WDATA, VLVD_NODIR|VLVF_PUB_RW, 0, 1 ,69,0);
    __Vscopep_x__u->varsInsertFromTable(Vx_leaf__VpiVarTable1, 1, &(TOP__x__u));
"#;

    fn site(path: &str, width: u32, writable: bool) -> ProbeSite {
        let (scope, var) = path.rsplit_once('.').unwrap();
        ProbeSite {
            path: path.into(),
            module: "m".into(),
            scope: scope.into(),
            var: var.into(),
            width,
            writable,
        }
    }

    #[test]
    fn the_table_gives_each_scope_its_variables_and_widths() {
        let s = parse(SYMS);
        let widths: Vec<_> = s.0["x"]
            .iter()
            .map(|v| (v.name.as_str(), v.width))
            .collect();
        assert_eq!(
            widths,
            [
                ("r", Some(8)),
                ("one", Some(1)),
                ("two", Some(32)),
                ("un", Some(8)),
                ("re", Some(1)),
                ("st", Some(1)),
                ("odd", Some(8)),
                ("LP", Some(70))
            ]
        );
        let x = &s.0["x"];
        assert!(x[7].param && x[3].unpacked && !x[0].writable);
        let types: Vec<&str> = x.iter().map(|v| v.vltype.as_str()).collect();
        assert_eq!(
            types[4..],
            ["VLVT_REAL", "VLVT_STRING", "VLVT_UINT16", "VLVT_WDATA"]
        );
        assert_eq!(s.0["x.u"][0].width, Some(10));
    }

    /// `--output-split` puts the insert calls in `__Syms__ctor__<n>__Slow.cpp` files, which
    /// may be read before the tables and scope rows of `__Syms__Slow.cpp`.
    #[test]
    fn calls_before_their_tables_and_scopes_resolve() {
        let (decls, calls) = SYMS.split_at(SYMS.find("    __Vscopep_x->").unwrap());
        let reordered = format!("{calls}\n{decls}");
        let s = parse(&reordered);
        let names =
            |scope: &str| -> Vec<String> { s.0[scope].iter().map(|v| v.name.clone()).collect() };
        assert_eq!(
            names("x"),
            ["r", "one", "two", "un", "re", "st", "odd", "LP"]
        );
        assert_eq!(names("x.u"), ["s"]);
        assert!(!s.0.contains_key(""), "every call found its scope");
    }

    #[test]
    fn mismatches_name_what_is_wrong() {
        let s = parse(SYMS);
        let msg = |p: ProbeSite| check(&s, &p).unwrap_err();
        assert!(
            msg(site("x.r", 5, false))
                .contains("`r` is 8 bits wide in the design, but the probe's type is 5 bits")
        );
        assert!(msg(site("x.rr", 8, false)).contains("did you mean `r`?"));
        assert!(msg(site("x.v.r", 8, false)).contains("no scope `x.v`"));
        assert!(msg(site("x.un", 8, false)).contains("unpacked"));
        assert!(msg(site("x.LP", 70, true)).contains("parameter"));
        assert!(msg(site("x.r", 8, true)).contains("not public_flat_rw"));
        // Not packed: the table's one bit is no width to suggest.
        assert!(msg(site("x.re", 1, false)).starts_with(
            "`re` is a `real` (Verilator stores it as VLVT_REAL); only packed variables"
        ));
        assert!(msg(site("x.st", 8, false)).starts_with("`st` is a `string`"));
        assert!(msg(site("x.odd", 8, false)).contains("stores it as VLVT_UINT16"));
        for ok in [
            site("x.r", 8, false),
            site("x.LP", 70, false),
            site("x.u.s", 10, true),
        ] {
            assert_eq!(check(&s, &ok), Ok(()));
        }
    }
}
