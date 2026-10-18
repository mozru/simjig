//! The sweep lowering: several [`Lowered`] configurations of one DUT module in, one
//! [`LoweredSweep`] out.
//!
//! Every member is lowered on its own first (its names, slots and types are its own);
//! this module only decides what the sweep module holds:
//! - **shared items**: a generated type, package-constant module or bundle that is equal
//!   (same Rust name and definition) in every member, and names only shared types, is
//!   defined once in the sweep module and imported by each member (`pub use super::X`),
//!   so `small::Item` and `big::Item` are one Rust type. The check is a fixpoint: a
//!   struct equal everywhere but holding a type that differs is not shared;
//! - **the `Ports` trait**: one method per `Dut` field that every member has with the
//!   same access, returning the field. A field whose type is equal in every member keeps
//!   that type; the others get an associated type, one per distinct list of
//!   per-member types, so ports that agree in every member (`wr_data` and `rd_data`, both
//!   `WIDTH` bits) share it and a generic body can move a value between them. It is
//!   named after the generated type it is in every member (`Word`), else after its first
//!   port (`WrData`), and bounded by `simjig::Vector` (bit vectors everywhere),
//!   `simjig::Value` (packed values) or nothing (unpacked arrays). The DUT parameters
//!   every member has with one Rust type are associated consts;
//! - **bundle traits**: an interface port whose bundle differs between members gets an
//!   associated type bounded by a trait built the same way over the bundles' signals.
//!   Each bundle trait's associated types are `Ports`' too, and the bound fixes them
//!   (`type Up: BeatIfSinkPorts<Beat = Self::Beat>`), so the same type in two bundle traits
//!   (`up.beat`, `down.beat`) is one type in a generic body, `D::Beat`;
//! - **fields traits**: an associated type that is a packed struct in every member, with
//!   the same field names, is bounded by `<Name>Fields` (`BeatFields`, which extends
//!   `simjig::Value`): a getter per field, `new(field, ..)` and `with_<field>`, the field
//!   types built like the signals' (concrete or associated). The fields trait's associated
//!   types are `Ports`' (and a bundle trait's) too, recursively through nested structs,
//!   fixed by the bound (`type Beat: BeatFields<Data = Self::Din, ..>`), so a field and a
//!   port of one type in every member (`beat.data()`, `dout`) are one type in a generic
//!   body. A fields trait's own types are its fields' only (`OuterFields::Inner` is
//!   bounded by `InnerFields` alone): a body over `Ports` sees them tied through it;
//! - **method names**: a trait method named like one of its supertraits' (`clone`, `fmt`;
//!   for a fields trait also `Value`'s, `new` and another field's setter) or a blanket
//!   impl's (`into`, `from`, `to_owned`, ...) gets `_`
//!   appended (`clone_`), and its docs say so; a fields trait's associated type named
//!   like a supertrait's (`Packed::Raw`) is numbered (`Raw2`);
//! - **runtime traits**: `Value`, `Vector` and `Packed`, re-exported by the sweep module
//!   unless a shared type has the name, so a generic body's `use <sweep>::*;` brings every
//!   bound into scope.
//!
//! Fields not in every member (a probe of one configuration) are left out of the trait,
//! and listed in its docs.

use std::collections::BTreeSet;

use super::super::names::{Namespace, camel};
use super::super::{
    Access, AssocBound, AssocType, Bundle, ConstGroup, ConstValue, DutField, Error, FieldsTrait,
    Item, ItemKind, Lowered, LoweredSweep, MethodKind, PortsTrait, RustType, StructField,
    SweepMember, TraitConst, TraitField, TraitMethod, TraitType,
};

/// The name of the sweep's trait over every member's `Dut`.
pub const PORTS_TRAIT: &str = "Ports";

/// The runtime traits whose methods a generic body calls on the associated types, which
/// the sweep module re-exports: `Value` (`get`/`set` bounds, `to_u128`), `Vector`, and
/// `Packed` (`::WIDTH`).
const RUNTIME_TRAITS: &[&str] = &["Value", "Vector", "Packed"];

/// Lowers the sweep `name` of `members` (each a module name and its lowering).
///
/// # Errors
/// [`Error::Config`] when there are no members, when they configure different DUT
/// modules, or when two names of the sweep module collide (a member named like a shared
/// type or package).
pub fn lower_sweep(name: &str, members: &[(String, Lowered)]) -> Result<LoweredSweep, Error> {
    let item = || format!("sweep `{name}`");
    let Some((_, first)) = members.first() else {
        return Err(Error::config(item(), "a sweep needs at least one DUT"));
    };
    let dut = dut_module(first).to_owned();
    if let Some((other, l)) = members.iter().find(|(_, l)| dut_module(l) != dut) {
        return Err(Error::config(
            item(),
            format!(
                "every DUT of a sweep configures one module, but `{}` is `{dut}` and `{other}` \
                 is `{}`",
                members[0].0,
                dut_module(l)
            ),
        ));
    }
    let shared = Shared::find(members);
    let mut types = Namespace::with_reserved(&[PORTS_TRAIT]);
    for n in &shared.names {
        types.claim(n);
    }
    let runtime_traits: Vec<String> = RUNTIME_TRAITS
        .iter()
        .filter(|n| !shared.names.contains(**n))
        .map(|n| types.claim(n))
        .collect();
    let mut builder = Builder {
        members,
        shared: &shared,
        types,
        bundle_traits: Vec::new(),
        fields_traits: Vec::new(),
    };
    let ports = builder.ports_trait(&dut);
    let Builder {
        bundle_traits,
        fields_traits,
        types,
        ..
    } = builder;
    check_member_names(name, members, &types)?;
    Ok(LoweredSweep {
        name: name.to_owned(),
        dut,
        members: members
            .iter()
            .map(|(n, l)| SweepMember {
                name: n.clone(),
                params: l.dut_params.clone(),
            })
            .collect(),
        reexports: shared.reexports(),
        items: shared.items,
        packages: shared.packages,
        bundles: shared.bundles,
        ports,
        bundle_traits,
        fields_traits,
        runtime_traits,
    })
}

/// The DUT module of a lowering: the wrapped one for an interface-ported DUT.
fn dut_module(lowered: &Lowered) -> &str {
    lowered.wrapper.as_ref().map_or(&lowered.top, |w| &w.dut)
}

/// A member module named like a type or module of the sweep module would collide with it.
fn check_member_names(
    name: &str,
    members: &[(String, Lowered)],
    types: &Namespace,
) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for (member, _) in members {
        if !seen.insert(member.as_str()) {
            return Err(Error::config(
                format!("sweep `{name}`"),
                format!("DUT `{member}` is in the sweep twice"),
            ));
        }
        if types.contains(member) || member == name {
            return Err(Error::config(
                format!("sweep `{name}`"),
                format!(
                    "DUT `{member}` is named like the sweep or an item of the sweep module; \
                     rename the DUT"
                ),
            ));
        }
    }
    Ok(())
}

/// What the members have in common.
struct Shared {
    items: Vec<Item>,
    packages: Vec<ConstGroup>,
    bundles: Vec<Bundle>,
    /// Every name the sweep module defines for them.
    names: BTreeSet<String>,
}

impl Shared {
    fn find(members: &[(String, Lowered)]) -> Self {
        let first = &members[0].1;
        let in_all = |f: &dyn Fn(&Lowered) -> bool| members.iter().all(|(_, l)| f(l));
        let mut items: Vec<Item> = first
            .items
            .iter()
            .filter(|it| it.name != PORTS_TRAIT && in_all(&|l| l.items.contains(it)))
            .cloned()
            .collect();
        // A type is shared only if every type it names is: drop until nothing changes.
        loop {
            let names: BTreeSet<&str> = items.iter().map(|i| i.name.as_str()).collect();
            let keep: Vec<bool> = items
                .iter()
                .map(|i| item_refs(i).iter().all(|r| names.contains(r.as_str())))
                .collect();
            if keep.iter().all(|&k| k) {
                break;
            }
            let mut keep = keep.into_iter();
            items.retain(|_| keep.next().unwrap_or(false));
        }
        let item_names: BTreeSet<String> = items.iter().map(|i| i.name.clone()).collect();
        let named_shared = |ty: &RustType| refs(ty).iter().all(|r| item_names.contains(r));
        let packages: Vec<ConstGroup> = first
            .packages
            .iter()
            .filter(|g| in_all(&|l| l.packages.contains(g)))
            .filter(|g| {
                g.consts.iter().all(|c| match &c.value {
                    ConstValue::Enum { ty, .. } => item_names.contains(ty),
                    _ => true,
                })
            })
            .cloned()
            .collect();
        let bundles: Vec<Bundle> = first
            .bundles
            .iter()
            .filter(|b| in_all(&|l| l.bundles.contains(b)))
            .filter(|b| b.members.iter().all(|m| named_shared(&m.ty)))
            .cloned()
            .collect();
        let mut shared = Self {
            items,
            packages,
            bundles,
            names: BTreeSet::new(),
        };
        shared.names = shared.reexports().into_iter().collect();
        shared
    }

    /// The names a member imports from the sweep module, in definition order.
    fn reexports(&self) -> Vec<String> {
        let mut names = Vec::new();
        for item in &self.items {
            names.push(item.name.clone());
            if let ItemKind::Enum { kind_name, .. } = &item.kind {
                names.push(kind_name.clone());
            }
        }
        names.extend(self.packages.iter().map(|g| g.module.clone()));
        names.extend(self.bundles.iter().map(|b| b.name.clone()));
        names
    }

    fn has_item(&self, name: &str) -> bool {
        self.items.iter().any(|i| i.name == name)
    }

    fn has_bundle(&self, bundle: &Bundle) -> bool {
        self.bundles.contains(bundle)
    }
}

/// The generated types `item` names.
fn item_refs(item: &Item) -> Vec<String> {
    match &item.kind {
        ItemKind::Alias { target } => refs(target),
        ItemKind::Struct { fields } => fields.iter().flat_map(|f| refs(&f.ty)).collect(),
        ItemKind::Union { bits, views } => refs(bits)
            .into_iter()
            .chain(views.iter().flat_map(|v| refs(&v.ty)))
            .collect(),
        ItemKind::Enum { base, .. } => refs(base),
    }
}

/// The generated types `ty` names.
fn refs(ty: &RustType) -> Vec<String> {
    match ty {
        RustType::Named(n) => vec![n.clone()],
        RustType::Array(elem, _) => refs(elem),
        RustType::Bit | RustType::Bits(_) | RustType::WideBits { .. } => Vec::new(),
    }
}

/// One implementation of a trait being built: the type, and its fields.
struct Column<'a> {
    /// The implementing type's path from the sweep module.
    path: String,
    /// The member it belongs to.
    member: &'a str,
    lowered: &'a Lowered,
    entries: Vec<Entry<'a>>,
}

/// A field of an implementing type.
struct Entry<'a> {
    name: &'a str,
    /// As the docs name it: a port on the user's module, not the generated wrapper.
    sv_name: String,
    kind: EntryKind<'a>,
}

enum EntryKind<'a> {
    Signal { access: Access, ty: &'a RustType },
    Bundle(&'a Bundle),
}

/// The member of each implementation: its module name and lowering.
type Owners<'a> = [(&'a str, &'a Lowered)];

/// A type of every member: one Rust type, or what its associated type needs.
enum Typed {
    Concrete(RustType),
    Assoc {
        name: String,
        bound: AssocBound,
        per_impl: Vec<RustType>,
    },
}

/// Methods every `Ports` and bundle trait has from its supertraits (`Copy`, `Debug`), and
/// every type has from the standard library's blanket impls (`Into`, `From`, `TryInto`,
/// `TryFrom`, `ToOwned`, `Borrow`, `BorrowMut`): a trait method of one of these names
/// makes a call like `x.into()` ambiguous (E0034).
const HANDLE_METHODS: &[&str] = &[
    "clone",
    "clone_from",
    "fmt",
    "into",
    "from",
    "try_into",
    "try_from",
    "to_owned",
    "clone_into",
    "borrow",
    "borrow_mut",
];

/// Methods a [`FieldsTrait`] has from its supertraits (`Value` and its own), the blanket
/// impls ([`HANDLE_METHODS`]) and itself.
const VALUE_METHODS: &[&str] = &[
    "clone",
    "clone_from",
    "fmt",
    "into",
    "from",
    "try_into",
    "try_from",
    "to_owned",
    "clone_into",
    "borrow",
    "borrow_mut",
    "eq",
    "ne",
    "default",
    "to_raw",
    "from_raw",
    "to_u128",
    "from_u128",
    "new",
];

/// The associated types a [`FieldsTrait`] has from its supertraits (`Packed::Raw`, through
/// `Value`): a field type of that name would make `Self::Raw` ambiguous (E0221).
const VALUE_TYPES: &[&str] = &["Raw"];

/// A field's method name: its own, or with `_` appended when it would be one of `taken`.
fn method_name(field: &str, taken: &[&str]) -> String {
    if taken.contains(&field) {
        format!("{field}_")
    } else {
        field.to_owned()
    }
}

/// Builds the traits, claiming their names in the sweep module's type namespace.
struct Builder<'a> {
    members: &'a [(String, Lowered)],
    shared: &'a Shared,
    types: Namespace,
    bundle_traits: Vec<PortsTrait>,
    fields_traits: Vec<FieldsTrait>,
}

impl<'a> Builder<'a> {
    fn ports_trait(&mut self, dut: &str) -> PortsTrait {
        let columns: Vec<Column<'a>> = self
            .members
            .iter()
            .map(|(member, l)| Column {
                path: format!("{member}::Dut"),
                member,
                lowered: l,
                entries: dut_entries(l),
            })
            .collect();
        let mut t = self.build(PORTS_TRAIT.to_owned(), dut.to_owned(), &columns);
        t.consts = self.consts();
        t
    }

    /// The DUT parameters every member has with one Rust type.
    fn consts(&self) -> Vec<TraitConst> {
        let first = &self.members[0].1;
        first
            .dut_params
            .iter()
            .filter(|c| {
                if let ConstValue::Enum { ty, .. } = &c.value
                    && !self.shared.has_item(ty)
                {
                    return false;
                }
                self.members.iter().all(|(_, l)| {
                    l.dut_params
                        .iter()
                        .any(|o| o.name == c.name && const_type(&o.value) == const_type(&c.value))
                })
            })
            .map(|c| TraitConst {
                name: c.name.clone(),
                sv_name: c.sv_name.clone(),
                value: c.value.clone(),
            })
            .collect()
    }

    /// A trait with one method per field every column has alike.
    fn build(&mut self, name: String, sv_name: String, columns: &[Column<'a>]) -> PortsTrait {
        let mut t = PortsTrait {
            name,
            sv_name,
            impls: columns.iter().map(|c| c.path.clone()).collect(),
            consts: Vec::new(),
            types: Vec::new(),
            methods: Vec::new(),
            omitted: Vec::new(),
        };
        let owners: Vec<(&'a str, &'a Lowered)> =
            columns.iter().map(|c| (c.member, c.lowered)).collect();
        let mut assoc = Namespace::default();
        for entry in &columns[0].entries {
            let same: Option<Vec<&Entry<'a>>> = columns
                .iter()
                .map(|c| {
                    c.entries
                        .iter()
                        .find(|e| e.name == entry.name)
                        .filter(|e| alike(e, entry))
                })
                .collect();
            let Some(same) = same else {
                t.omitted.push(entry.name.to_owned());
                continue;
            };
            let kind = match &entry.kind {
                EntryKind::Signal { access, .. } => {
                    let raw: Vec<&'a RustType> = same
                        .iter()
                        .map(|e| match e.kind {
                            EntryKind::Signal { ty, .. } => ty,
                            EntryKind::Bundle(_) => unreachable!("`alike` keeps kinds equal"),
                        })
                        .collect();
                    let ty = match self.typed(&owners, &raw, entry.name) {
                        Typed::Concrete(ty) => TraitType::Concrete(ty),
                        Typed::Assoc {
                            name,
                            bound,
                            per_impl,
                        } => {
                            self.place(&mut t.types, &mut assoc, &name, bound, per_impl, entry.name)
                        }
                    };
                    MethodKind::Signal {
                        access: *access,
                        ty,
                    }
                }
                EntryKind::Bundle(_) => {
                    MethodKind::Bundle(self.bundle_type(&mut t, &mut assoc, columns, &same))
                }
            };
            t.methods.push(TraitMethod {
                name: method_name(entry.name, HANDLE_METHODS),
                field: entry.name.to_owned(),
                sv_name: entry.sv_name.clone(),
                kind,
            });
        }
        t
    }

    /// The type `raw[i]` of member `owners[i]`, in a trait over all of them: concrete if
    /// equal everywhere, else an associated type named after the generated type it is in
    /// every member (`Word`), or else after `user`, and bounded by what every member's
    /// type is (a packed struct by its [`FieldsTrait`]).
    fn typed(&mut self, owners: &Owners<'a>, raw: &[&'a RustType], user: &str) -> Typed {
        let per_impl: Vec<RustType> = owners
            .iter()
            .zip(raw)
            .map(|((member, _), ty)| self.scoped(ty, member))
            .collect();
        if per_impl.iter().all(|ty| *ty == per_impl[0]) {
            return Typed::Concrete(per_impl[0].clone());
        }
        let bound = owners
            .iter()
            .zip(raw)
            .map(|((_, l), ty)| class(ty, l))
            .reduce(weaker)
            .unwrap_or(AssocBound::None);
        let base = match &per_impl[0] {
            RustType::Named(n) => Some(n.rsplit("::").next().unwrap_or(n).to_owned()),
            _ => None,
        }
        .filter(|base| {
            per_impl
                .iter()
                .all(|ty| matches!(ty, RustType::Named(n) if n.rsplit("::").next() == Some(base)))
        });
        let name = base.unwrap_or_else(|| camel(user.trim_start_matches("r#")));
        let bound = match bound {
            AssocBound::Value => self
                .fields_trait(&name, owners, raw, &per_impl)
                .map_or(AssocBound::Value, AssocBound::Trait),
            other => other,
        };
        Typed::Assoc {
            name,
            bound,
            per_impl,
        }
    }

    /// The [`FieldsTrait`] of the packed struct `raw[i]` of each member (as `per_impl`
    /// names them from the sweep module), when every member's has the same field names;
    /// reused when another type has the same structs.
    fn fields_trait(
        &mut self,
        base: &str,
        owners: &Owners<'a>,
        raw: &[&'a RustType],
        per_impl: &[RustType],
    ) -> Option<String> {
        let impls: Vec<String> = per_impl
            .iter()
            .map(|ty| match ty {
                RustType::Named(n) => Some(n.clone()),
                _ => None,
            })
            .collect::<Option<_>>()?;
        if let Some(f) = self.fields_traits.iter().find(|f| f.impls == impls) {
            return Some(f.name.clone());
        }
        let structs: Vec<(&'a Item, &'a [StructField])> = owners
            .iter()
            .zip(raw)
            .map(|((_, l), ty)| struct_fields(ty, l))
            .collect::<Option<_>>()?;
        let (item, first) = structs[0];
        let same_names = structs.iter().all(|(_, fields)| {
            fields.len() == first.len() && fields.iter().zip(first).all(|(a, b)| a.name == b.name)
        });
        if !same_names {
            return None;
        }
        let mut t = FieldsTrait {
            name: self.types.claim(&format!("{base}Fields")),
            sv_name: item.sv_name.clone(),
            impls,
            types: Vec::new(),
            fields: Vec::new(),
        };
        let setters: Vec<String> = first
            .iter()
            .map(|f| format!("with_{}", f.name.trim_start_matches("r#")))
            .collect();
        let mut taken: Vec<&str> = VALUE_METHODS.to_vec();
        taken.extend(setters.iter().map(String::as_str));
        let mut assoc = Namespace::with_reserved(VALUE_TYPES);
        for (i, field) in first.iter().enumerate() {
            let field_raw: Vec<&'a RustType> = structs.iter().map(|(_, f)| &f[i].ty).collect();
            let ty = match self.typed(owners, &field_raw, &field.name) {
                Typed::Concrete(ty) => TraitType::Concrete(ty),
                Typed::Assoc {
                    name,
                    bound,
                    per_impl,
                } => assoc_type(
                    &mut t.types,
                    &mut assoc,
                    &name,
                    bound,
                    per_impl,
                    &field.name,
                ),
            };
            t.fields.push(TraitField {
                name: field.name.clone(),
                getter: method_name(&field.name, &taken),
                setter: setters[i].clone(),
                sv_name: field.sv_name.clone(),
                ty,
            });
        }
        let name = t.name.clone();
        self.fields_traits.push(t);
        Some(name)
    }

    /// An interface port's type: the shared bundle, or an associated type bounded by a
    /// bundle trait over the members' bundles. The bundle trait's own associated types
    /// are associated types of `t` too, which the bound fixes them to, so ports of two
    /// bundle traits carrying one type (`up.beat`, `down.beat`) share it in `t`.
    fn bundle_type(
        &mut self,
        t: &mut PortsTrait,
        assoc: &mut Namespace,
        columns: &[Column<'a>],
        same: &[&Entry<'a>],
    ) -> TraitType {
        let bundles: Vec<&'a Bundle> = same
            .iter()
            .map(|e| match e.kind {
                EntryKind::Bundle(b) => b,
                EntryKind::Signal { .. } => unreachable!("`alike` keeps kinds equal"),
            })
            .collect();
        if bundles.iter().all(|b| *b == bundles[0]) && self.shared.has_bundle(bundles[0]) {
            return TraitType::Concrete(RustType::Named(bundles[0].name.clone()));
        }
        let per_impl: Vec<RustType> = columns
            .iter()
            .zip(&bundles)
            .map(|(c, b)| RustType::Named(format!("{}::{}", c.member, b.name)))
            .collect();
        let existing = self.bundle_traits.iter().position(|bt| {
            bt.impls
                .iter()
                .zip(&per_impl)
                .all(|(p, ty)| *ty == RustType::Named(p.clone()))
        });
        let index = existing.unwrap_or_else(|| {
            let name = self.types.claim(&format!("{}Ports", bundles[0].name));
            let sub: Vec<Column<'a>> = columns
                .iter()
                .zip(&bundles)
                .map(|(c, b)| Column {
                    path: format!("{}::{}", c.member, b.name),
                    member: c.member,
                    lowered: c.lowered,
                    entries: bundle_entries(b),
                })
                .collect();
            let bt = self.build(name, bundles[0].sv_name.clone(), &sub);
            self.bundle_traits.push(bt);
            self.bundle_traits.len() - 1
        });
        let port = same[0].name;
        let inner = self.bundle_traits[index].types.clone();
        let equals = lift(&mut t.types, assoc, inner, port);
        let name = camel(port.trim_start_matches("r#"));
        let bound = AssocBound::Trait(self.bundle_traits[index].name.clone());
        let ty = assoc_type(&mut t.types, assoc, &name, bound, per_impl, port);
        if let TraitType::Assoc(i) = ty {
            t.types[i].equals = equals;
        }
        ty
    }

    /// The associated type of a `Ports` or bundle trait with these per-implementation types,
    /// as [`assoc_type`] makes it. One bounded by a [`FieldsTrait`] also brings that trait's
    /// associated types into `types`, recursively, and the bound fixes them
    /// (`type Outer: OuterFields<Inner = Self::Inner>`), as a bundle trait's are: so a port
    /// and a struct field of one type in every member (`dout`, `beat.data`) are one type in
    /// a generic body. A fields trait's own types stay its fields' only: a body generic
    /// over the fields trait alone does not need the nested ones.
    fn place(
        &self,
        types: &mut Vec<AssocType>,
        assoc: &mut Namespace,
        name: &str,
        bound: AssocBound,
        per_impl: Vec<RustType>,
        user: &str,
    ) -> TraitType {
        let fields = match &bound {
            AssocBound::Trait(n) => self.fields_traits.iter().find(|f| f.name == *n),
            _ => None,
        }
        .map(|f| f.types.clone());
        let ty = assoc_type(types, assoc, name, bound, per_impl, user);
        let (Some(inner), TraitType::Assoc(i)) = (fields, &ty) else {
            return ty;
        };
        let mut equals = Vec::new();
        for a in inner {
            // Each user of the field's type is a user here (`o.i`, and `io` elsewhere).
            let mut placed = None;
            for u in &a.users {
                let at = format!("{user}.{u}");
                let ty = self.place(
                    types,
                    assoc,
                    &a.name,
                    a.bound.clone(),
                    a.per_impl.clone(),
                    &at,
                );
                placed = Some(ty);
            }
            let Some(TraitType::Assoc(j)) = placed else {
                unreachable!("an associated type has a user, and `place` makes one")
            };
            equals.push((a.name, j));
        }
        types[*i].equals = equals;
        ty
    }

    /// `ty` as named from the sweep module: a shared type by its name, a member's own
    /// type through the member's module.
    fn scoped(&self, ty: &RustType, member: &str) -> RustType {
        match ty {
            RustType::Named(n) if !self.shared.has_item(n) => {
                RustType::Named(format!("{member}::{n}"))
            }
            RustType::Array(elem, len) => {
                RustType::Array(Box::new(self.scoped(elem, member)), *len)
            }
            other => other.clone(),
        }
    }
}

/// The struct item `ty` is in `lowered` (through aliases), and its fields.
fn struct_fields<'l>(ty: &RustType, lowered: &'l Lowered) -> Option<(&'l Item, &'l [StructField])> {
    let RustType::Named(n) = ty else {
        return None;
    };
    let item = lowered.items.iter().find(|i| i.name == *n)?;
    match &item.kind {
        ItemKind::Alias { target } => struct_fields(target, lowered),
        ItemKind::Struct { fields } => Some((item, fields)),
        _ => None,
    }
}

/// The associated type with these per-implementation types (reused when another field
/// has them), with `user` recorded.
fn assoc_type(
    types: &mut Vec<AssocType>,
    assoc: &mut Namespace,
    name: &str,
    bound: AssocBound,
    per_impl: Vec<RustType>,
    user: &str,
) -> TraitType {
    if let Some(i) = types.iter().position(|a| a.per_impl == per_impl) {
        types[i].users.push(user.to_owned());
        return TraitType::Assoc(i);
    }
    types.push(AssocType {
        name: assoc.claim(name),
        bound,
        per_impl,
        users: vec![user.to_owned()],
        equals: Vec::new(),
    });
    TraitType::Assoc(types.len() - 1)
}

/// A bundle trait's associated types `inner` as associated types of the trait holding the
/// interface port `port`, with their users prefixed by it: the bundle trait's bound's
/// `equals` (`Beat = Self::Beat`), each type's own `equals` remapped to `types`.
fn lift(
    types: &mut Vec<AssocType>,
    assoc: &mut Namespace,
    inner: Vec<AssocType>,
    port: &str,
) -> Vec<(String, usize)> {
    let map: Vec<usize> = inner
        .iter()
        .map(|a| {
            let users: Vec<String> = a.users.iter().map(|u| format!("{port}.{u}")).collect();
            let ty = assoc_type(
                types,
                assoc,
                &a.name,
                a.bound.clone(),
                a.per_impl.clone(),
                &users[0],
            );
            let TraitType::Assoc(i) = ty else {
                unreachable!("`assoc_type` makes an associated type")
            };
            types[i].users.extend(users.into_iter().skip(1));
            i
        })
        .collect();
    for (a, &i) in inner.iter().zip(&map) {
        if !a.equals.is_empty() {
            types[i].equals = a.equals.iter().map(|(n, j)| (n.clone(), map[*j])).collect();
        }
    }
    inner
        .into_iter()
        .zip(map)
        .map(|(a, i)| (a.name, i))
        .collect()
}

/// Whether two fields of the same name can be one trait method.
fn alike(a: &Entry<'_>, b: &Entry<'_>) -> bool {
    match (&a.kind, &b.kind) {
        (EntryKind::Signal { access: x, .. }, EntryKind::Signal { access: y, .. }) => x == y,
        (EntryKind::Bundle(_), EntryKind::Bundle(_)) => true,
        _ => false,
    }
}

/// The fields of a member's `Dut`.
fn dut_entries(l: &Lowered) -> Vec<Entry<'_>> {
    let on_dut = |s: &str| match s.strip_prefix(&format!("{}.", l.top)) {
        Some(port) => format!("{}.{port}", dut_module(l)),
        None => s.to_owned(),
    };
    l.fields
        .iter()
        .map(|f| match f {
            DutField::Signal { name, slot } => {
                let s = &l.signals[*slot as usize];
                Entry {
                    name,
                    sv_name: on_dut(&s.name),
                    kind: EntryKind::Signal {
                        access: s.access,
                        ty: &s.ty,
                    },
                }
            }
            DutField::Bundle { name, bundle, .. } => {
                let b = &l.bundles[*bundle];
                Entry {
                    name,
                    sv_name: b.sv_name.clone(),
                    kind: EntryKind::Bundle(b),
                }
            }
        })
        .collect()
}

/// The fields of a bundle.
fn bundle_entries(b: &Bundle) -> Vec<Entry<'_>> {
    b.members
        .iter()
        .map(|m| Entry {
            name: &m.name,
            sv_name: m.sv_name.clone(),
            kind: EntryKind::Signal {
                access: m.access,
                ty: &m.ty,
            },
        })
        .collect()
}

/// What a generic body may assume of `ty` in `lowered`.
fn class(ty: &RustType, lowered: &Lowered) -> AssocBound {
    match ty {
        RustType::Bit | RustType::Bits(_) | RustType::WideBits { .. } => AssocBound::Vector,
        RustType::Array(..) => AssocBound::None,
        RustType::Named(n) => match lowered.items.iter().find(|i| i.name == *n) {
            Some(Item {
                kind: ItemKind::Alias { target },
                ..
            }) => class(target, lowered),
            Some(Item {
                storage: Some(_), ..
            }) => AssocBound::Value,
            _ => AssocBound::None,
        },
    }
}

/// The bound both `a` and `b` satisfy.
fn weaker(a: AssocBound, b: AssocBound) -> AssocBound {
    match (a, b) {
        (AssocBound::Vector, AssocBound::Vector) => AssocBound::Vector,
        (AssocBound::Vector | AssocBound::Value, AssocBound::Vector | AssocBound::Value) => {
            AssocBound::Value
        }
        _ => AssocBound::None,
    }
}

/// The Rust type of a constant, as a comparable key.
fn const_type(value: &ConstValue) -> String {
    match value {
        ConstValue::Int { ty, .. } => (*ty).to_owned(),
        ConstValue::Wide { width, .. } => format!("wide{width}"),
        ConstValue::Enum { ty, .. } => format!("enum {ty}"),
        ConstValue::Str(_) => "str".to_owned(),
        ConstValue::Real(_) => "f64".to_owned(),
    }
}
