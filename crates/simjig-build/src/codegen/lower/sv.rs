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

use svtypes::ir::{Design, TypeId, TypeKind};

use super::super::Error;

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
    Ok((spell_packed(design, id, mode)?, unpacked))
}

/// The spelling of a packed type (or a nameable unpacked one).
pub(crate) fn spell_packed(design: &Design, id: TypeId, mode: Mode) -> Result<String, Error> {
    let (base, dims) = base_and_dims(design, id, mode)?;
    Ok(if dims.is_empty() {
        base
    } else {
        format!("{base} {}", dims.concat())
    })
}

/// Splits a packed type into its element spelling and packed dimensions, outermost first:
/// `logic [3:0][2:0]` is `("logic", ["[3:0]", "[2:0]"])`.
fn base_and_dims(design: &Design, id: TypeId, mode: Mode) -> Result<(String, Vec<String>), Error> {
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
            let (mut base, inner) = base_and_dims(design, *elem, mode)?;
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
            (
                spell_aggregate(design, word, *signed, fields, mode)?,
                Vec::new(),
            )
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
            return base_and_dims(design, *base, mode);
        }
        TypeKind::Struct { packed: false, .. }
        | TypeKind::Union { packed: false, .. }
        | TypeKind::UnpackedArray { .. } => {
            return Err(Error::unsupported(
                describe(design, id),
                "unpacked aggregates inside packed types do not exist in SystemVerilog",
            ));
        }
    })
}

/// `struct packed [signed] { T a; ... }` (or `union`).
fn spell_aggregate(
    design: &Design,
    word: &str,
    signed: bool,
    fields: &[svtypes::ir::Field],
    mode: Mode,
) -> Result<String, Error> {
    let sign = if signed { " signed" } else { "" };
    let members = fields
        .iter()
        .map(|f| Ok(format!("{} {};", spell_packed(design, f.ty, mode)?, f.name)))
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(format!("{word} packed{sign} {{ {} }}", members.join(" ")))
}

/// A short description of a type for error messages.
pub(crate) fn describe(design: &Design, id: TypeId) -> String {
    match &design.ty(id).name {
        Some(name) => format!("type {name}"),
        None => format!("anonymous type #{}", id.0),
    }
}
