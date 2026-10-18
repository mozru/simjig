//! Rust types for IR types: which IR types become generated items, their names, and the
//! Rust type expression of every other type.
//!
//! Item names come from the SystemVerilog typedef name with one conventional suffix
//! stripped (`beat_t` is `Beat`, `op_e` is `Op`). Names that collide fall back, for every
//! type in the collision, to the unstripped name, then to the scope-qualified name, then to
//! a numbered name, so no type silently takes another's nicer name. Anonymous types are
//! named after where they are first used (`top.in_anon` is `InAnon`), in a traversal that
//! follows the IR's declaration order.

use std::collections::BTreeMap;

use svtypes::ir::{Design, TypeId, TypeKind};
use svtypes::width::packed_storage;

use super::super::error::UNKNOWN_IR_KIND;
use super::super::names::{Namespace, camel, screaming, snake};
use super::super::{EnumVariant, Error, Item, ItemKind, RustType, StructField, UnionView};

/// Type names the generated code must not shadow: the `Dut` and the prelude.
pub(crate) const RESERVED_TYPES: &[&str] = &[
    "Dut",
    "Self",
    "Option",
    "Some",
    "None",
    "Result",
    "Ok",
    "Err",
    "Box",
    "String",
    "Vec",
    "Copy",
    "Clone",
    "Debug",
    "Default",
    "Eq",
    "PartialEq",
    "Ord",
    "PartialOrd",
    "Hash",
    "Send",
    "Sync",
    "Sized",
    "Unpin",
    "Drop",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "Into",
    "TryFrom",
    "TryInto",
    "Iterator",
    "IntoIterator",
    "Extend",
    "AsRef",
    "AsMut",
    "ToOwned",
    "ToString",
];

/// Suffixes stripped from typedef names (`_t` type, `_e` enum, `_s` struct, `_u` union).
const SUFFIXES: &[&str] = &["_t", "_e", "_s", "_u"];

/// Where a type is used, for naming an anonymous type and for error messages.
#[derive(Clone, Debug)]
pub(crate) struct Use {
    /// Human-readable location, e.g. `top.in_anon` or `pkg::outer_t.inner`.
    pub(crate) path: String,
    /// Name candidates in preference order.
    pub(crate) candidates: Vec<String>,
}

/// A root of the type traversal, with its use site.
#[derive(Clone, Debug)]
pub(crate) struct Root {
    pub(crate) ty: TypeId,
    pub(crate) site: Use,
    /// An unsupported type here is an error (ports, probes), not a silent skip (package
    /// typedefs nobody uses).
    pub(crate) required: bool,
}

/// The Rust view of the design's types.
#[derive(Debug, Default)]
pub(crate) struct Types {
    /// Item name per generated type id.
    names: BTreeMap<TypeId, String>,
    /// The generated items, by type id.
    pub(crate) items: Vec<(TypeId, Item)>,
    /// Every name taken in the crate-level type namespace.
    pub(crate) namespace: Namespace,
}

impl Types {
    /// The Rust type expression for `id`.
    pub(crate) fn rust(&self, design: &Design, id: TypeId) -> RustType {
        if let Some(name) = self.names.get(&id) {
            return RustType::Named(name.clone());
        }
        let ty = design.ty(id);
        match (&ty.kind, ty.width) {
            (TypeKind::UnpackedArray { elem, range }, _) => {
                // `count` only fails above u32::MAX elements, which no IR can hold.
                let count = range.count().unwrap_or(u32::MAX);
                RustType::Array(Box::new(self.rust(design, *elem)), count)
            }
            (_, Some(width)) => bits(width),
            // Unpacked structs and unions never reach here: `generates` rejects them.
            (_, None) => RustType::Bits(0),
        }
    }
}

/// The bit-vector type for `width` bits.
pub(crate) fn bits(width: u32) -> RustType {
    match width {
        1 => RustType::Bit,
        2..=128 => RustType::Bits(width),
        _ => RustType::WideBits {
            width,
            words: width.div_ceil(32),
        },
    }
}

/// Whether `id` becomes a generated item, or an error if it cannot be represented.
fn generates(design: &Design, id: TypeId, path: &str) -> Result<bool, Error> {
    let ty = design.ty(id);
    match &ty.kind {
        TypeKind::Struct { packed: false, .. } | TypeKind::Union { packed: false, .. } => {
            Err(Error::unsupported(
                path,
                "unpacked structs and unions have no Verilator port storage",
            ))
        }
        TypeKind::Enum { .. } if ty.width.is_some_and(|w| w > 128) => {
            Err(Error::unsupported(path, "enums wider than 128 bits"))
        }
        TypeKind::Struct { .. } | TypeKind::Union { .. } | TypeKind::Enum { .. } => Ok(true),
        TypeKind::Bits { .. } | TypeKind::PackedArray { .. } | TypeKind::UnpackedArray { .. } => {
            Ok(ty.name.is_some() && ty.alias_of.is_some())
        }
        _ => Err(Error::unsupported(path, UNKNOWN_IR_KIND)),
    }
}

/// Name candidates for a named type `scope::name`.
pub(crate) fn named_candidates(qualified: &str) -> Vec<String> {
    let (scope, name) = qualified.rsplit_once("::").unwrap_or(("", qualified));
    let scope = scope.trim_start_matches('$');
    let stem = SUFFIXES
        .iter()
        .find_map(|s| name.strip_suffix(s).filter(|rest| !rest.is_empty()))
        .unwrap_or(name);
    let mut out = vec![camel(stem), camel(name), camel(&format!("{scope}_{name}"))];
    out.dedup();
    out
}

/// The type ids reachable from `roots`, each with its first use, in traversal order.
fn reach(design: &Design, roots: &[Root]) -> Result<BTreeMap<TypeId, Use>, Error> {
    let mut seen: BTreeMap<TypeId, Use> = BTreeMap::new();
    for root in roots {
        let mut stack = vec![(root.ty, root.site.clone())];
        let mut found = Vec::new();
        let mut ok = true;
        while let Some((id, site)) = stack.pop() {
            if seen.contains_key(&id) || found.iter().any(|(f, _)| *f == id) {
                continue;
            }
            if let Err(e) = generates(design, id, &site.path) {
                if root.required {
                    return Err(e);
                }
                ok = false;
                break;
            }
            push_children(design, id, &site, &mut stack);
            found.push((id, site));
        }
        // A package typedef that cannot be represented is left out with everything it
        // reaches, unless a port or probe needs it.
        if ok {
            seen.extend(found);
        }
    }
    Ok(seen)
}

/// Pushes the types `id` refers to, last first, so they pop in declaration order.
fn push_children(design: &Design, id: TypeId, site: &Use, stack: &mut Vec<(TypeId, Use)>) {
    let ty = design.ty(id);
    let parent = ty
        .name
        .as_deref()
        .map_or_else(|| site.path.clone(), str::to_owned);
    let parent_stems: Vec<String> = match &ty.name {
        Some(n) => named_candidates(n),
        None => site.candidates.clone(),
    };
    let child = |part: &str| Use {
        path: format!("{parent}.{part}"),
        candidates: parent_stems
            .iter()
            .map(|s| camel(&format!("{s}_{part}")))
            .collect(),
    };
    let mut children: Vec<(TypeId, Use)> = Vec::new();
    match &ty.kind {
        TypeKind::Struct { fields, .. } | TypeKind::Union { fields, .. } => {
            children.extend(fields.iter().map(|f| (f.ty, child(&f.name))));
        }
        TypeKind::Enum { base, .. } => children.push((*base, child("base"))),
        TypeKind::PackedArray { elem, .. } | TypeKind::UnpackedArray { elem, .. } => {
            children.push((*elem, child("elem")));
        }
        // `generates` refused a kind it does not know before its children are wanted.
        _ => {}
    }
    if let Some(target) = ty.alias_of {
        children.push((target, site.clone()));
    }
    stack.extend(children.into_iter().rev());
}

/// Resolves item names by the fallback rule in the module docs.
fn resolve_names(cands: &[(TypeId, Vec<String>)], reserved: &Namespace) -> Vec<String> {
    let mut level = vec![0usize; cands.len()];
    loop {
        let names: Vec<String> = cands
            .iter()
            .zip(&level)
            .map(|((id, c), &l)| match c.get(l) {
                Some(name) => name.clone(),
                None => format!("{}{}", c.last().map_or("Type", String::as_str), id.0),
            })
            .collect();
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        for n in &names {
            *count.entry(n).or_default() += 1;
        }
        let mut bumped = false;
        for (i, n) in names.iter().enumerate() {
            let clash = count[n.as_str()] > 1 || reserved.contains(n);
            if clash && level[i] <= cands[i].1.len() {
                level[i] += 1;
                bumped = true;
            }
        }
        if !bumped {
            return names;
        }
    }
}

/// Lowers every type reachable from `roots`.
pub(crate) fn lower_types(design: &Design, roots: &[Root]) -> Result<Types, Error> {
    let reached = reach(design, roots)?;
    let generated: Vec<(TypeId, &Use)> = reached
        .iter()
        .filter(|(id, _)| generates(design, **id, "").unwrap_or(false))
        .map(|(id, site)| (*id, site))
        .collect();
    let cands: Vec<(TypeId, Vec<String>)> = generated
        .iter()
        .map(|(id, site)| {
            let c = match &design.ty(*id).name {
                Some(n) => named_candidates(n),
                None => site.candidates.clone(),
            };
            (*id, c)
        })
        .collect();
    let mut types = Types {
        namespace: Namespace::with_reserved(RESERVED_TYPES),
        ..Types::default()
    };
    let names = resolve_names(&cands, &types.namespace);
    for ((id, _), name) in generated.iter().zip(&names) {
        types.namespace.claim(name);
        types.names.insert(*id, name.clone());
    }
    for ((id, site), name) in generated.iter().zip(names) {
        let item = lower_item(design, &mut types, *id, site, name)?;
        types.items.push((*id, item));
    }
    Ok(types)
}

/// Builds the item for type `id`.
fn lower_item(
    design: &Design,
    types: &mut Types,
    id: TypeId,
    site: &Use,
    name: String,
) -> Result<Item, Error> {
    let ty = design.ty(id);
    let sv_name = ty.name.clone().unwrap_or_else(|| site.path.clone());
    let width = ty.width.unwrap_or(0);
    let storage = ty
        .width
        .map(packed_storage)
        .transpose()
        .map_err(|e| Error::unsupported(&sv_name, e.0))?;
    let kind = if let Some(target) = ty.alias_of {
        ItemKind::Alias {
            target: types.rust(design, target),
        }
    } else {
        match &ty.kind {
            TypeKind::Struct { fields, .. } => {
                let mut ns = Namespace::default();
                let fields = fields
                    .iter()
                    .map(|f| StructField {
                        name: ns.claim(&snake(&f.name)),
                        sv_name: f.name.clone(),
                        ty: types.rust(design, f.ty),
                        lsb: f.lsb.unwrap_or(0),
                        width: design.ty(f.ty).width.unwrap_or(0),
                        is_struct: matches!(design.ty(f.ty).kind, TypeKind::Struct { .. }),
                    })
                    .collect();
                ItemKind::Struct { fields }
            }
            TypeKind::Union { fields, .. } => {
                let mut ns = Namespace::with_reserved(&["bits", "to_bits", "raw", "to_raw"]);
                let views = fields
                    .iter()
                    .map(|f| {
                        let getter = ns.claim(&snake(&f.name));
                        let constructor = format!("from_{}", getter.trim_start_matches("r#"));
                        UnionView {
                            getter,
                            constructor,
                            sv_name: f.name.clone(),
                            ty: types.rust(design, f.ty),
                        }
                    })
                    .collect();
                ItemKind::Union {
                    bits: bits(width),
                    views,
                }
            }
            TypeKind::Enum { variants, .. } => {
                let kind_name = types.namespace.claim(&format!("{name}Variant"));
                ItemKind::Enum {
                    base: bits(width),
                    kind_name,
                    variants: lower_variants(variants),
                }
            }
            TypeKind::Bits { .. }
            | TypeKind::PackedArray { .. }
            | TypeKind::UnpackedArray { .. } => unreachable_alias(&sv_name)?,
            _ => return Err(Error::unsupported(&sv_name, UNKNOWN_IR_KIND)),
        }
    };
    Ok(Item {
        name,
        sv_name,
        width,
        storage,
        kind,
    })
}

/// A non-alias Bits/array type is never generated (`generates`); reaching here is a bug.
fn unreachable_alias(sv_name: &str) -> Result<ItemKind, Error> {
    Err(Error::unsupported(
        sv_name,
        "internal: a vector type without alias was lowered as an item",
    ))
}

/// Variant names: consts and kind variants, with a common `PREFIX_` stripped when every
/// variant has it and every remainder still starts with a letter (`OP_ADD` is `Op::ADD`).
fn lower_variants(variants: &[svtypes::ir::Variant]) -> Vec<EnumVariant> {
    let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
    let prefix = common_prefix(&names);
    let mut consts = Namespace::with_reserved(&["WIDTH"]);
    let mut kinds = Namespace::default();
    variants
        .iter()
        .map(|v| {
            let stem = &v.name[prefix..];
            let words = v.value.words();
            let value = u128::from(words.first().copied().unwrap_or(0))
                | (u128::from(words.get(1).copied().unwrap_or(0)) << 64);
            EnumVariant {
                sv_name: v.name.clone(),
                const_name: consts.claim(&screaming(stem)),
                kind_name: kinds.claim(&camel(stem)),
                value,
            }
        })
        .collect()
}

/// The length of the `WORD_` prefix shared by every name, or 0.
fn common_prefix(names: &[&str]) -> usize {
    let [first, _, ..] = names else { return 0 };
    let mut best = 0;
    for (i, _) in first.match_indices('_') {
        let len = i + 1;
        let shared = names.iter().all(|n| {
            n.len() > len
                && n.as_bytes()[..len] == first.as_bytes()[..len]
                && n.as_bytes()[len].is_ascii_alphabetic()
        });
        if !shared {
            break;
        }
        best = len;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_stripped_only_at_word_boundaries() {
        assert_eq!(common_prefix(&["OP_NOP", "OP_ADD"]), 3);
        assert_eq!(common_prefix(&["A_B_C", "A_B_D"]), 4);
        assert_eq!(common_prefix(&["X0", "X1"]), 0);
        assert_eq!(common_prefix(&["MODE_1", "MODE_2"]), 0);
        assert_eq!(common_prefix(&["ONLY_ONE"]), 0);
        assert_eq!(common_prefix(&["S_A", "T_A"]), 0);
    }

    #[test]
    fn candidates_strip_one_suffix() {
        assert_eq!(
            named_candidates("pkg::beat_t"),
            ["Beat", "BeatT", "PkgBeatT"]
        );
        assert_eq!(named_candidates("$unit::word"), ["Word", "UnitWord"]);
        assert_eq!(named_candidates("pkg::_t"), ["T", "PkgT"]);
    }

    /// The examples of the README's "Generated names" section, so it cannot drift from the
    /// code generator.
    #[test]
    fn the_readme_naming_examples_hold() {
        for (typedef, item) in [
            ("pkg::status_t", "Status"),
            ("pkg::op_e", "Op"),
            ("pkg::item_s", "Item"),
            ("pkg::word_u", "Word"),
            ("pkg::rgb", "Rgb"),
        ] {
            assert_eq!(named_candidates(typedef)[0], item, "{typedef}");
        }
        assert_eq!(
            named_candidates("a::state_t"),
            ["State", "StateT", "AStateT"]
        );
        let variants = ["ST_EMPTY", "ST_FULL"];
        let stem = &variants[0][common_prefix(&variants)..];
        assert_eq!(
            (screaming(stem), camel(stem)),
            ("EMPTY".into(), "Empty".into())
        );
        assert_eq!(common_prefix(&["MODE_1", "MODE_2"]), 0, "`1` is not a name");
        for (port, field) in [
            ("in", "r#in"),
            ("type", "r#type"),
            ("self", "self_"),
            ("dataIn", "data_in"),
            ("DATA_OUT", "data_out"),
            ("2nd", "_2nd"),
        ] {
            assert_eq!(snake(port), field, "{port}");
        }
        assert_eq!(camel("in_anon"), "InAnon");
    }

    #[test]
    fn collisions_fall_back_for_every_member() {
        let cands = vec![
            (TypeId(0), named_candidates("a::state_t")),
            (TypeId(1), named_candidates("b::state_t")),
            (TypeId(2), named_candidates("a::dut_t")),
        ];
        let names = resolve_names(&cands, &Namespace::with_reserved(RESERVED_TYPES));
        assert_eq!(names, ["AStateT", "BStateT", "DutT"]);
    }
}
