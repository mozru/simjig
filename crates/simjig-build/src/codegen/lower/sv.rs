//! SystemVerilog spellings of IR types, for the layout check and the wrapper.
//!
//! A type declared in a package or in `$unit` is spelled by its qualified name, which SV
//! code outside the design can use. Anonymous types and types declared inside a module
//! cannot be named from outside, so they are spelled out from their structure. A spelled
//! struct or union is a new type with the same layout (IEEE 1800-2023 §7.2.1 fixes the
//! layout from the member list), which is what the layout check needs. An enum that cannot
//! be named is spelled as its base type in the layout check (same layout, and no second
//! declaration of its constants); the wrapper rejects it ([`Mode::Wrapper`]), because a
//! base vector is not assignment compatible with the enum (§6.19.3).
//!
//! In the layout check every spelled-out aggregate becomes a local typedef
//! ([`LayoutSpeller`]), nested ones first, and is referred to by name. Verilator 5.052
//! cannot take an aggregate type written inline in an expression such as `$bits(..)` when
//! it nests another one (`V3LinkParse`: "Implicit enum/struct type created under unexpected
//! node type"); a typedef is always accepted. The wrapper keeps inline spellings, which
//! only appear in port declarations.

use svtypes::ir::{Design, TypeId, TypeKind};

use super::super::error::UNKNOWN_IR_KIND;
use super::super::{Error, LayoutTypedef};

/// Who the spelling is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The layout check: anything spellable is fine.
    Layout,
    /// A wrapper port: the spelling must be assignment compatible with the original type.
    Wrapper,
}

/// Whether `name` (`scope::t`) is declared in a package or `$unit`, so it is nameable.
pub(crate) fn nameable(design: &Design, name: &str) -> bool {
    match name.rsplit_once("::") {
        Some(("$unit", _)) => true,
        Some((scope, _)) => design.packages.iter().any(|p| p.name == scope),
        None => false,
    }
}

/// The spelling of `id` as `(packed part, unpacked dimensions)`, e.g.
/// `("logic [7:0]", "[0:3]")` for `logic [7:0] x [4]`.
pub(crate) fn spell(design: &Design, id: TypeId, mode: Mode) -> Result<(String, String), Error> {
    let mut unpacked = String::new();
    let mut id = id;
    while let TypeKind::UnpackedArray { elem, range } = &design.ty(id).kind {
        if design
            .ty(id)
            .name
            .as_deref()
            .is_some_and(|n| nameable(design, n))
        {
            break;
        }
        unpacked.push_str(&range.to_string());
        id = *elem;
    }
    // Wrapper ports declare their aggregates inline, so no typedefs are collected.
    Ok((spell_packed(design, id, mode, &mut Vec::new())?, unpacked))
}

/// The spelling of a packed type (or a nameable unpacked one).
fn spell_packed(
    design: &Design,
    id: TypeId,
    mode: Mode,
    defs: &mut Vec<(TypeId, LayoutTypedef)>,
) -> Result<String, Error> {
    let (base, dims) = base_and_dims(design, id, mode, defs)?;
    Ok(if dims.is_empty() {
        base
    } else {
        format!("{base} {}", dims.concat())
    })
}

/// Spells types for the layout check, collecting a typedef per spelled-out aggregate.
#[derive(Debug, Default)]
pub(crate) struct LayoutSpeller {
    /// Each typedef with the IR type it names, dependencies first.
    defs: Vec<(TypeId, LayoutTypedef)>,
}

impl LayoutSpeller {
    /// The layout-check spelling of packed type `id`: a name wherever an aggregate occurs.
    pub(crate) fn spell(&mut self, design: &Design, id: TypeId) -> Result<String, Error> {
        spell_packed(design, id, Mode::Layout, &mut self.defs)
    }

    /// The typedefs, in declaration order.
    pub(crate) fn into_typedefs(self) -> Vec<LayoutTypedef> {
        self.defs.into_iter().map(|(_, d)| d).collect()
    }
}

/// Splits a packed type into its element spelling and packed dimensions, outermost first:
/// `logic [3:0][2:0]` is `("logic", ["[3:0]", "[2:0]"])`.
fn base_and_dims(
    design: &Design,
    id: TypeId,
    mode: Mode,
    defs: &mut Vec<(TypeId, LayoutTypedef)>,
) -> Result<(String, Vec<String>), Error> {
    let ty = design.ty(id);
    if let Some(name) = ty.name.as_deref().filter(|n| nameable(design, n)) {
        return Ok((name.to_owned(), Vec::new()));
    }
    let keyword = if ty.four_state { "logic" } else { "bit" };
    Ok(match &ty.kind {
        TypeKind::Bits { signed, range, .. } => {
            let base = if *signed {
                format!("{keyword} signed")
            } else {
                keyword.to_owned()
            };
            (base, range.iter().map(ToString::to_string).collect())
        }
        TypeKind::PackedArray {
            elem,
            range,
            signed,
        } => {
            let (mut base, inner) = base_and_dims(design, *elem, mode, defs)?;
            if *signed && (base == "logic" || base == "bit") {
                base.push_str(" signed");
            }
            let mut dims = vec![range.to_string()];
            dims.extend(inner);
            (base, dims)
        }
        TypeKind::Struct {
            packed: true,
            signed,
            fields,
        }
        | TypeKind::Union {
            packed: true,
            signed,
            fields,
        } => {
            let word = if matches!(ty.kind, TypeKind::Struct { .. }) {
                "struct"
            } else {
                "union"
            };
            let decl = spell_aggregate(design, word, *signed, fields, mode, defs)?;
            (name_aggregate(id, decl, mode, defs), Vec::new())
        }
        TypeKind::Enum { base, .. } => {
            if mode == Mode::Wrapper {
                return Err(Error::unsupported(
                    describe(design, id),
                    "an anonymous or module-local enum cannot be named outside the DUT, so \
                     the wrapper cannot declare a port of that type; move the typedef into a \
                     package",
                ));
            }
            // An enum is stored as its base type. Spelling the enum itself would declare
            // its constants again in the checking scope.
            return base_and_dims(design, *base, mode, defs);
        }
        TypeKind::Struct { packed: false, .. }
        | TypeKind::Union { packed: false, .. }
        | TypeKind::UnpackedArray { .. } => {
            return Err(Error::unsupported(
                describe(design, id),
                "unpacked aggregates inside packed types do not exist in SystemVerilog",
            ));
        }
        _ => return Err(Error::unsupported(describe(design, id), UNKNOWN_IR_KIND)),
    })
}

/// `struct packed [signed] { T a; ... }` (or `union`).
fn spell_aggregate(
    design: &Design,
    word: &str,
    signed: bool,
    fields: &[svtypes::ir::Field],
    mode: Mode,
    defs: &mut Vec<(TypeId, LayoutTypedef)>,
) -> Result<String, Error> {
    let sign = if signed { " signed" } else { "" };
    let members = fields
        .iter()
        .map(|f| {
            let ty = spell_packed(design, f.ty, mode, defs)?;
            Ok(format!("{ty} {};", f.name))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(format!("{word} packed{sign} {{ {} }}", members.join(" ")))
}

/// In the layout check, the typedef name standing for aggregate `id` spelled as `decl`
/// (declared once per IR type); in the wrapper, `decl` itself.
fn name_aggregate(
    id: TypeId,
    decl: String,
    mode: Mode,
    defs: &mut Vec<(TypeId, LayoutTypedef)>,
) -> String {
    if mode == Mode::Wrapper {
        return decl;
    }
    if let Some((_, def)) = defs.iter().find(|(t, _)| *t == id) {
        return def.name.clone();
    }
    // Type ids are unique, and the prefix keeps the name clear of the design's own.
    let name = format!("simjig_t{}", id.0);
    defs.push((
        id,
        LayoutTypedef {
            name: name.clone(),
            decl,
        },
    ));
    name
}

/// A short description of a type for error messages.
pub(crate) fn describe(design: &Design, id: TypeId) -> String {
    match &design.ty(id).name {
        Some(name) => format!("type {name}"),
        None => format!("anonymous type #{}", id.0),
    }
}
