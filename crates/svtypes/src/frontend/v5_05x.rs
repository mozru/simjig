//! Decoder for the `--json-only` dialect of Verilator 5.050 to 5.059.
//!
//! Facts about this dialect (checked against 5.052 output, see `docs/notes/svtypes.md`):
//! - Types live in `/miscsp/*/typesp` (the `TYPETABLE`) and are linked by `addr`.
//! - There is no width field. `BASICDTYPE` has a `range` string (`"7:0"`, absent for a
//!   1-bit scalar); arrays have a `declRange` string (`"[3:0]"`).
//! - `MEMBERDTYPE.lsb` is a string and is omitted when 0. We compute offsets ourselves and
//!   use it only as a cross-check.
//! - `REFDTYPE` is one step of a typedef chain (`refDTypep`); `TYPEDEF` nodes in modules and
//!   packages give each typedef its scope.
//! - Parameterised units are cloned per parameter set: `name` is mangled (`sub__N3`),
//!   `origName` is the source name, encoded like every Verilator identifier (`__024` = `$`).
//! - The top module has `level` 1.
//!
//! Strictness: every dtype node reached from a port, parameter or typedef is checked
//! field by field; an unknown node type or field is an error naming its JSON path.

use std::collections::BTreeMap;

use serde_json::Value;

use super::json::{Index, Node};
use crate::error::{Error, Result};
use crate::ir::{
    Design, Direction, Field, IfacePort, Instance, Modport, ModportMember, Module, ModuleId,
    ModuleKind, Package, Param, ParamValue, Port, Producer, Range, SCHEMA, Storage, TimeExp, Type,
    TypeId, TypeKind, Unrepresentable, Variable, VariableKind, Variant,
};
use crate::literal::{self, Literal};
use crate::width;

/// Decodes one `.tree.json` / `.tree.meta.json` pair. `version` is `major.minor`.
pub(crate) fn decode(tree: &Value, meta: &Value, version: &str) -> Result<Design> {
    let ix = Index::new(tree, meta, version);
    let root = Node {
        obj: tree.as_object().ok_or_else(|| Error::Json {
            path: "/".into(),
            source_loc: None,
            version: version.into(),
            message: "the document is not a JSON object".into(),
        })?,
        path: "".into(),
    };
    if Index::kind(&root) != "NETLIST" {
        return Err(ix.err(&root, "the root node is not a NETLIST"));
    }
    let precision = ix.str(&root, "timeprecision")?;
    let time_precision: TimeExp = precision.parse().map_err(|e: String| ix.err(&root, e))?;

    let mut units = Vec::new();
    let mut packages = Vec::new();
    for node in ix.children(&root, "modulesp")? {
        match Index::kind(&node) {
            "MODULE" | "IFACE" => units.push(node),
            // `std` is Verilator's built-in package (classes, not design types).
            "PACKAGE" if ix.str(&node, "origName")? != "std" => packages.push(node),
            // Other units (classes, the std package) are not reachable from ports.
            _ => {}
        }
    }
    let mut d = Decoder::new(ix, &units, &packages)?;

    let mut out_packages = Vec::new();
    for p in &packages {
        out_packages.push(d.package(p)?);
    }
    out_packages.sort_by(|a, b| a.name.cmp(&b.name));

    let tops: Vec<_> = d.units.iter().filter(|u| u.level == 1).collect();
    let [top] = tops.as_slice() else {
        return Err(d.ix.err(
            &root,
            format!("expected one top module (level 1), found {}", tops.len()),
        ));
    };
    let top_id = top.id;
    let mut modules = Vec::new();
    for i in 0..d.units.len() {
        let unit = d.units[i].clone();
        modules.push(d.module(&unit, unit.id == top_id)?);
    }
    Ok(Design {
        schema: SCHEMA,
        producer: Producer {
            tool: "verilator".into(),
            version: version.into(),
        },
        time_precision,
        top: top_id,
        types: d.types,
        modules,
        packages: out_packages,
    })
}

/// A module or interface node with its assigned id.
#[derive(Clone, Debug)]
struct Unit<'a> {
    node: Node<'a>,
    id: ModuleId,
    level: i64,
}

/// Where a `typedef` was declared.
#[derive(Debug)]
struct TypedefSite<'a> {
    scope: String,
    name: &'a str,
    /// The `addr` the typedef names.
    target: &'a str,
}

struct Decoder<'a> {
    ix: Index<'a>,
    /// Units sorted by (orig name, name); position equals [`ModuleId`].
    units: Vec<Unit<'a>>,
    unit_ids: BTreeMap<&'a str, ModuleId>,
    typedefs: Vec<TypedefSite<'a>>,
    types: Vec<Type>,
    interned: BTreeMap<Type, TypeId>,
    by_addr: BTreeMap<&'a str, TypeId>,
}

impl<'a> Decoder<'a> {
    fn new(ix: Index<'a>, units: &[Node<'a>], packages: &[Node<'a>]) -> Result<Self> {
        let mut keyed = Vec::new();
        for node in units {
            let orig = decode_name(ix.str(node, "origName")?);
            let name = ix.str(node, "name")?;
            let level = ix.opt_int(node, "level")?.unwrap_or(0);
            keyed.push(((orig, name), node.clone(), level));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        let mut units = Vec::new();
        let mut unit_ids = BTreeMap::new();
        for (i, (_, node, level)) in keyed.into_iter().enumerate() {
            let id = ModuleId(
                u32::try_from(i).map_err(|e| ix.err(&node, format!("too many modules: {e}")))?,
            );
            unit_ids.insert(ix.addr(&node)?, id);
            units.push(Unit { node, id, level });
        }
        let mut typedefs = Vec::new();
        for scope_node in units.iter().map(|u| &u.node).chain(packages) {
            let scope = decode_name(ix.str(scope_node, "origName")?);
            for stmt in ix.children(scope_node, "stmtsp")? {
                if Index::kind(&stmt) == "TYPEDEF" {
                    typedefs.push(TypedefSite {
                        scope: scope.clone(),
                        name: ix.str(&stmt, "name")?,
                        target: ix.str(&stmt, "dtypep")?,
                    });
                }
            }
        }
        Ok(Decoder {
            ix,
            units,
            unit_ids,
            typedefs,
            types: Vec::new(),
            interned: BTreeMap::new(),
            by_addr: BTreeMap::new(),
        })
    }

    fn package(&mut self, node: &Node<'a>) -> Result<Package> {
        let name = decode_name(self.ix.str(node, "origName")?);
        let mut params = Vec::new();
        let mut unrepresentable = Vec::new();
        let mut typedefs = Vec::new();
        for stmt in self.ix.children(node, "stmtsp")? {
            match Index::kind(&stmt) {
                "VAR" if matches!(self.ix.str(&stmt, "varType")?, "LPARAM" | "GPARAM") => {
                    match self.param(&stmt)? {
                        Ok(p) => params.push(p),
                        Err(u) => unrepresentable.push(u),
                    }
                }
                "TYPEDEF" if !is_synthetic(self.ix.str(&stmt, "name")?) => {
                    self.ix.check_fields(&stmt, &["attrsp"])?;
                    let target = self.ty(&self.ix.deref(&stmt, "dtypep")?)?;
                    let qualified = format!("{name}::{}", self.ix.str(&stmt, "name")?);
                    typedefs.push(self.named(target, qualified));
                }
                _ => {}
            }
        }
        Ok(Package {
            name,
            params,
            unrepresentable,
            typedefs,
        })
    }

    fn module(&mut self, unit: &Unit<'a>, is_top: bool) -> Result<Module> {
        let ix_kind = Index::kind(&unit.node);
        let kind = if ix_kind == "IFACE" {
            ModuleKind::Interface
        } else {
            ModuleKind::Module
        };
        let mut m = Module {
            name: self.ix.str(&unit.node, "name")?.to_owned(),
            orig_name: decode_name(self.ix.str(&unit.node, "origName")?),
            kind,
            params: Vec::new(),
            unrepresentable: Vec::new(),
            ports: Vec::new(),
            iface_ports: Vec::new(),
            modports: Vec::new(),
            instances: Vec::new(),
            variables: Vec::new(),
        };
        for stmt in self.ix.children(&unit.node, "stmtsp")? {
            self.hierarchy_item(&stmt, &mut m)?;
            match Index::kind(&stmt) {
                // A port is any non-interface VAR with a direction: `inout wire` ports
                // have varType WIRE, not PORT.
                "VAR" => match self.ix.str(&stmt, "varType")? {
                    "GPARAM" => match self.param(&stmt)? {
                        Ok(p) => m.params.push(p),
                        Err(u) => m.unrepresentable.push(u),
                    },
                    "IFACEREF" => {
                        if let Some(p) = self.iface_port(&stmt)? {
                            m.iface_ports.push(p);
                        }
                    }
                    _ if self.ix.str(&stmt, "direction")? != "NONE" => {
                        m.ports.push(self.port(&stmt, is_top)?);
                    }
                    _ => {}
                },
                "MODPORT" => m.modports.push(self.modport(&stmt)?),
                _ => {}
            }
        }
        Ok(m)
    }

    /// Records `stmt` in the module's instances or variables, if it is one of them. A cell
    /// whose unit is not a decoded module or interface (none in 5.052's elaborated output)
    /// is left out rather than failing the design: the hierarchy only serves diagnostics.
    fn hierarchy_item(&self, stmt: &Node<'a>, m: &mut Module) -> Result<()> {
        let ix = &self.ix;
        match Index::kind(stmt) {
            "CELL" => {
                if let Some(&module) = ix.opt_str(stmt, "modp")?.and_then(|a| self.unit_ids.get(a))
                {
                    m.instances.push(Instance {
                        name: decode_name(ix.str(stmt, "origName")?),
                        module,
                    });
                }
            }
            "VAR" => {
                let kind = match ix.str(stmt, "varType")? {
                    "IFACEREF" => return Ok(()),
                    "GPARAM" => VariableKind::Parameter,
                    "LPARAM" => VariableKind::Localparam,
                    _ => VariableKind::Signal,
                };
                m.variables.push(Variable {
                    name: decode_name(ix.str(stmt, "origName")?),
                    kind,
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Decodes a parameter; `Ok(Err(..))` for one whose value the IR cannot hold (an X or
    /// Z constant), which the caller records instead of failing the design.
    fn param(&mut self, var: &Node<'a>) -> Result<Result<Param, Unrepresentable>> {
        let ix = &self.ix;
        let name = ix.str(var, "name")?.to_owned();
        let value = ix.only_child(var, "valuep")?;
        if Index::kind(&value) != "CONST" {
            return Err(ix.err(
                &value,
                format!(
                    "parameter `{name}` has a {} value; only constants are supported",
                    Index::kind(&value)
                ),
            ));
        }
        let text = ix.str(&value, "name")?;
        let dtype = ix.deref(var, "dtypep")?;
        let resolved = self.resolve(&dtype)?;
        if Index::kind(&resolved) == "BASICDTYPE" {
            match self.ix.str(&resolved, "keyword")? {
                "string" => {
                    let s = unquote(text).ok_or_else(|| {
                        self.ix
                            .err(&value, format!("malformed string constant `{text}`"))
                    })?;
                    return Ok(Ok(Param {
                        name,
                        value: ParamValue::String(s),
                        ty: None,
                    }));
                }
                "real" | "realtime" | "shortreal" => {
                    return Ok(Ok(Param {
                        name,
                        value: ParamValue::Real(text.to_owned()),
                        ty: None,
                    }));
                }
                _ => {}
            }
        }
        let ty = self.ty(&dtype)?;
        if literal::is_four_state(text) {
            return Ok(Err(Unrepresentable {
                name,
                value: text.to_owned(),
                reason: "the value has X or Z bits, and IR values are two-state".to_owned(),
            }));
        }
        let lit = self.typed_literal(&value, ty)?;
        Ok(Ok(Param {
            name,
            value: ParamValue::Bits(lit),
            ty: Some(ty),
        }))
    }

    /// Parses the `CONST` node `value` as a value of type `ty`, whose width it must have.
    fn typed_literal(&self, value: &Node<'a>, ty: TypeId) -> Result<Literal> {
        let text = self.ix.str(value, "name")?;
        let lit: Literal = text
            .parse()
            .map_err(|e: literal::LiteralError| self.ix.err(value, e.to_string()))?;
        let t = &self.types[ty.index()];
        let width = t
            .width
            .ok_or_else(|| self.ix.err(value, "constant of an unpacked type"))?;
        if lit.width() != width {
            return Err(self.ix.err(
                value,
                format!("constant `{text}` is not {width} bits wide like its type"),
            ));
        }
        lit.retyped(width, self.signed(ty))
            .map_err(|e| self.ix.err(value, e.to_string()))
    }

    fn signed(&self, ty: TypeId) -> bool {
        match &self.types[ty.index()].kind {
            TypeKind::Bits { signed, .. }
            | TypeKind::Struct { signed, .. }
            | TypeKind::Union { signed, .. }
            | TypeKind::PackedArray { signed, .. } => *signed,
            TypeKind::Enum { base, .. } => self.signed(*base),
            TypeKind::UnpackedArray { .. } => false,
        }
    }

    /// Follows `REFDTYPE` and `PARAMTYPEDTYPE` links to the underlying dtype node.
    fn resolve(&self, node: &Node<'a>) -> Result<Node<'a>> {
        let mut node = node.clone();
        for _ in 0..256 {
            match Index::kind(&node) {
                "REFDTYPE" | "PARAMTYPEDTYPE" => node = self.ix.deref(&node, "dtypep")?,
                _ => return Ok(node),
            }
        }
        Err(self.ix.err(&node, "typedef chain does not terminate"))
    }

    fn direction(&self, node: &Node<'a>) -> Result<Direction> {
        match self.ix.str(node, "direction")? {
            "INPUT" => Ok(Direction::Input),
            "OUTPUT" => Ok(Direction::Output),
            "INOUT" => Ok(Direction::Inout),
            other => Err(self
                .ix
                .err(node, format!("unsupported port direction {other}"))),
        }
    }

    fn port(&mut self, var: &Node<'a>, is_top: bool) -> Result<Port> {
        self.ix.check_fields(var, VAR_FIELDS)?;
        let name = self.ix.str(var, "name")?.to_owned();
        let dir = self.direction(var)?;
        let ty = self
            .ty(&self.ix.deref(var, "dtypep")?)
            .map_err(|e| self.port_error(var, &name, e))?;
        let storage = self
            .storage(ty)
            .map_err(|e| self.ix.err(var, format!("port `{name}`: {e}")))?;
        let cpp_member = if is_top {
            Some(cpp_member(self.ix.str(var, "origName")?))
        } else {
            None
        };
        Ok(Port {
            name,
            dir,
            ty,
            cpp_member,
            storage,
        })
    }

    /// A type error reached from port `name`, reworded to lead with the port and its
    /// declaration (the type node may sit anywhere in the type table).
    fn port_error(&self, var: &Node<'a>, name: &str, e: Error) -> Error {
        match e {
            Error::Json {
                path,
                source_loc,
                version,
                message,
            } => Error::Json {
                path,
                source_loc: self.ix.source_loc(var).or(source_loc),
                version,
                message: format!("port `{name}`: {message}"),
            },
            other => other,
        }
    }

    fn storage(&self, ty: TypeId) -> std::result::Result<Storage, String> {
        let t = &self.types[ty.index()];
        match (&t.kind, t.width) {
            (TypeKind::UnpackedArray { elem, range }, _) => Ok(Storage::VlUnpacked {
                elem: Box::new(self.storage(*elem)?),
                len: range.count().map_err(|e| e.0)?,
            }),
            (_, Some(w)) => width::packed_storage(w).map_err(|e| e.0),
            (_, None) => Err(
                "unpacked structs and unions have no supported C++ storage; declare the type \
                 `packed`"
                    .into(),
            ),
        }
    }

    /// An interface port, or `None` for the handle Verilator adds for an interface
    /// instance (its `IFACEREFDTYPE` names the instantiating cell).
    fn iface_port(&mut self, var: &Node<'a>) -> Result<Option<IfacePort>> {
        let ix = &self.ix;
        let dtype = ix.deref(var, "dtypep")?;
        if Index::kind(&dtype) != "IFACEREFDTYPE" {
            return Err(ix.err(
                &dtype,
                format!("unsupported interface port type {}", Index::kind(&dtype)),
            ));
        }
        ix.check_fields(
            &dtype,
            &[
                "cellName",
                "ifaceName",
                "modportName",
                "ifacep",
                "cellp",
                "modportp",
            ],
        )?;
        if !ix.str(&dtype, "cellName")?.is_empty() {
            return Ok(None);
        }
        let iface = ix.deref(&dtype, "ifacep")?;
        let interface = *self
            .unit_ids
            .get(ix.addr(&iface)?)
            .ok_or_else(|| ix.err(&dtype, "`ifacep` is not an interface of this design"))?;
        let modport = ix.str(&dtype, "modportName")?;
        Ok(Some(IfacePort {
            name: ix.str(var, "name")?.to_owned(),
            interface,
            modport: (!modport.is_empty()).then(|| modport.to_owned()),
        }))
    }

    fn modport(&mut self, node: &Node<'a>) -> Result<Modport> {
        let mut members = Vec::new();
        for item in self.ix.children(node, "varsp")? {
            if Index::kind(&item) != "MODPORTVARREF" {
                return Err(self.ix.err(
                    &item,
                    format!("unsupported modport item {}", Index::kind(&item)),
                ));
            }
            self.ix.check_fields(&item, &["direction", "varp"])?;
            let var = self.ix.deref(&item, "varp")?;
            let ty = self.ty(&self.ix.deref(&var, "dtypep")?)?;
            members.push(ModportMember {
                name: self.ix.str(&item, "name")?.to_owned(),
                dir: self.direction(&item)?,
                ty,
            });
        }
        Ok(Modport {
            name: self.ix.str(node, "name")?.to_owned(),
            members,
        })
    }

    fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(&id) = self.interned.get(&ty) {
            return id;
        }
        let id = TypeId(u32::try_from(self.types.len()).unwrap_or(u32::MAX));
        self.types.push(ty.clone());
        self.interned.insert(ty, id);
        id
    }

    /// `target` under the typedef name `qualified`: `target` itself when it already has
    /// that name (Verilator names a struct or enum after its typedef), else an alias.
    fn named(&mut self, target: TypeId, qualified: String) -> TypeId {
        let t = &self.types[target.index()];
        if t.name.as_deref() == Some(qualified.as_str()) {
            return target;
        }
        let alias = Type {
            name: Some(qualified),
            alias_of: Some(target),
            four_state: t.four_state,
            width: t.width,
            kind: t.kind.clone(),
        };
        self.intern(alias)
    }

    /// The IR type of a dtype node.
    fn ty(&mut self, node: &Node<'a>) -> Result<TypeId> {
        let addr = self.ix.addr(node)?;
        if let Some(&id) = self.by_addr.get(addr) {
            return Ok(id);
        }
        let id = match Index::kind(node) {
            "BASICDTYPE" => self.basic(node)?,
            "REFDTYPE" => self.ref_dtype(node)?,
            "PARAMTYPEDTYPE" => {
                self.ix.check_fields(node, &["fwdType", "varType"])?;
                self.ty(&self.ix.deref(node, "dtypep")?)?
            }
            "ENUMDTYPE" => self.enum_dtype(node)?,
            "STRUCTDTYPE" | "UNIONDTYPE" => self.aggregate(node)?,
            "PACKARRAYDTYPE" | "UNPACKARRAYDTYPE" => self.array(node)?,
            other => {
                return Err(self
                    .ix
                    .err(node, format!("unsupported data type node {other}")));
            }
        };
        self.by_addr.insert(addr, id);
        Ok(id)
    }

    fn basic(&mut self, node: &Node<'a>) -> Result<TypeId> {
        self.ix
            .check_fields(node, &["keyword", "range", "generic", "signed"])?;
        let keyword = self.ix.str(node, "keyword")?;
        // IEEE 1800-2023 §6.11: integer and time are 4-state; Verilator folds reg/wire
        // into logic.
        let four_state = match keyword {
            "logic" | "integer" | "time" => true,
            "bit" | "byte" | "shortint" | "int" | "longint" => false,
            other => {
                return Err(self
                    .ix
                    .err(node, format!("unsupported basic type `{other}`")));
            }
        };
        let range = match self.ix.opt_str(node, "range")? {
            Some(text) => Some(text.parse::<Range>().map_err(|e| self.ix.err(node, e.0))?),
            None => None,
        };
        let w = match range {
            Some(r) => r.count().map_err(|e| self.ix.err(node, e.0))?,
            None => 1,
        };
        let signed = self.ix.flag(node, "signed")?;
        Ok(self.intern(Type {
            name: None,
            alias_of: None,
            four_state,
            width: Some(w),
            kind: TypeKind::Bits {
                width: w,
                signed,
                range,
            },
        }))
    }

    fn ref_dtype(&mut self, node: &Node<'a>) -> Result<TypeId> {
        self.ix.check_fields(
            node,
            &[
                "typedefp",
                "refDTypep",
                "classOrPackagep",
                "signed",
                "generic",
            ],
        )?;
        let next = self.ix.deref(node, "refDTypep")?;
        let target = self.ty(&next)?;
        if Index::kind(&next) == "PARAMTYPEDTYPE" {
            // A type parameter is not a typedef: use the type it was bound to.
            return Ok(target);
        }
        let name = self.ix.str(node, "name")?;
        if is_synthetic(name) {
            // Verilator's own typedef for an anonymous enum or struct port type.
            return Ok(target);
        }
        let next_addr = self.ix.addr(&next)?;
        let scope = match self
            .typedefs
            .iter()
            .find(|s| s.name == name && s.target == next_addr)
        {
            Some(site) => Some(site.scope.clone()),
            None => match self.ix.str(node, "classOrPackagep")? {
                "UNLINKED" => None,
                _ => Some(decode_name(
                    self.ix
                        .str(&self.ix.deref(node, "classOrPackagep")?, "origName")?,
                )),
            },
        };
        let qualified = scope.map_or_else(|| name.to_owned(), |s| format!("{s}::{name}"));
        Ok(self.named(target, qualified))
    }

    fn enum_dtype(&mut self, node: &Node<'a>) -> Result<TypeId> {
        self.ix.check_fields(
            node,
            &["enum", "refDTypep", "signed", "typedefName", "itemsp"],
        )?;
        let base = self.ty(&self.ix.deref(node, "refDTypep")?)?;
        let base_t = &self.types[base.index()];
        if !matches!(base_t.kind, TypeKind::Bits { .. }) {
            return Err(self
                .ix
                .err(node, "enum base type is not an integral vector"));
        }
        let (four_state, width) = (base_t.four_state, base_t.width);
        let mut variants = Vec::new();
        for item in self.ix.children(node, "itemsp")? {
            self.ix.check_fields(&item, &["valuep"])?;
            let value = self.ix.only_child(&item, "valuep")?;
            variants.push(Variant {
                name: self.ix.str(&item, "name")?.to_owned(),
                value: self.typed_literal(&value, base)?,
            });
        }
        Ok(self.intern(Type {
            name: type_name(self.ix.str(node, "name")?),
            alias_of: None,
            four_state,
            width,
            kind: TypeKind::Enum { base, variants },
        }))
    }

    fn aggregate(&mut self, node: &Node<'a>) -> Result<TypeId> {
        self.ix.check_fields(node, AGGREGATE_FIELDS)?;
        let packed = self.ix.flag(node, "packed")?;
        let is_union = Index::kind(node) == "UNIONDTYPE";
        let mut fields = Vec::new();
        let mut widths = Vec::new();
        let mut json_lsbs = Vec::new();
        let mut four_state = false;
        for member in self.ix.children(node, "membersp")? {
            self.ix
                .check_fields(&member, &["lsb", "tag", "refDTypep", "signed"])?;
            let ty = self.ty(&self.ix.deref(&member, "refDTypep")?)?;
            let t = &self.types[ty.index()];
            four_state |= t.four_state;
            if packed {
                widths.push(t.width.ok_or_else(|| {
                    self.ix
                        .err(&member, "packed aggregate member has an unpacked type")
                })?);
            }
            json_lsbs.push((
                member.clone(),
                self.ix.opt_str(&member, "lsb")?.unwrap_or("0"),
            ));
            fields.push(Field {
                name: self.ix.str(&member, "name")?.to_owned(),
                ty,
                lsb: None,
            });
        }
        let width = if packed {
            if self.ix.flag(node, "isFourstate")? != four_state {
                return Err(self.ix.err(
                    node,
                    format!("Verilator's isFourstate disagrees with the computed {four_state}"),
                ));
            }
            Some(self.packed_layout(node, is_union, &widths, &mut fields, &json_lsbs)?)
        } else {
            None
        };
        let signed = self.ix.flag(node, "signed")?;
        let kind = if is_union {
            TypeKind::Union {
                packed,
                signed,
                fields,
            }
        } else {
            TypeKind::Struct {
                packed,
                signed,
                fields,
            }
        };
        Ok(self.intern(Type {
            name: type_name(self.ix.str(node, "name")?),
            alias_of: None,
            four_state,
            width,
            kind,
        }))
    }

    /// Computes the width and field offsets of a packed struct or union, cross-checking
    /// each offset against the `lsb` Verilator printed.
    fn packed_layout(
        &self,
        node: &Node<'a>,
        is_union: bool,
        widths: &[u32],
        fields: &mut [Field],
        json_lsbs: &[(Node<'a>, &str)],
    ) -> Result<u32> {
        let (w, lsbs) = if is_union {
            (width::union_width(widths), Ok(vec![0; widths.len()]))
        } else {
            (width::struct_width(widths), width::struct_lsbs(widths))
        };
        let w = w.map_err(|e| self.ix.err(node, e.0))?;
        let lsbs = lsbs.map_err(|e| self.ix.err(node, e.0))?;
        for ((field, lsb), (member, text)) in fields.iter_mut().zip(lsbs).zip(json_lsbs) {
            if text.parse::<u32>().ok() != Some(lsb) {
                return Err(self
                    .ix
                    .err(member, format!("Verilator says lsb {text}, computed {lsb}")));
            }
            field.lsb = Some(lsb);
        }
        Ok(w)
    }

    fn array(&mut self, node: &Node<'a>) -> Result<TypeId> {
        self.ix
            .check_fields(node, &["declRange", "refDTypep", "signed", "rangep"])?;
        let elem = self.ty(&self.ix.deref(node, "refDTypep")?)?;
        let range: Range = self
            .ix
            .str(node, "declRange")?
            .parse()
            .map_err(|e: width::WidthError| self.ix.err(node, e.0))?;
        let e = &self.types[elem.index()];
        let four_state = e.four_state;
        let ty = if Index::kind(node) == "PACKARRAYDTYPE" {
            let ew = e
                .width
                .ok_or_else(|| self.ix.err(node, "packed array of an unpacked type"))?;
            let w = width::packed_array_width(ew, range).map_err(|e| self.ix.err(node, e.0))?;
            let signed = self.ix.flag(node, "signed")?;
            Type {
                name: None,
                alias_of: None,
                four_state,
                width: Some(w),
                kind: TypeKind::PackedArray {
                    elem,
                    range,
                    signed,
                },
            }
        } else {
            range.count().map_err(|e| self.ix.err(node, e.0))?;
            Type {
                name: None,
                alias_of: None,
                four_state,
                width: None,
                kind: TypeKind::UnpackedArray { elem, range },
            }
        };
        Ok(self.intern(ty))
    }
}

/// Fields of a `STRUCTDTYPE` or `UNIONDTYPE`. `isSoft` and `isTagged` unions are
/// deliberately absent, so they are rejected.
const AGGREGATE_FIELDS: &[&str] = &[
    "packed",
    "isFourstate",
    "classOrPackagep",
    "signed",
    "typedefName",
    "membersp",
];

/// Fields of a port `VAR` in this dialect. Anything else is unknown and rejected.
const VAR_FIELDS: &[&str] = &[
    "origName",
    "verilogName",
    "isPrimaryIO",
    "isPrimaryClock",
    "direction",
    "declDirection",
    "isConst",
    "isSigPublic",
    "isSigUserRdPublic",
    "isSigUserRWPublic",
    "icoMaybeWritten",
    "lifetime",
    "varType",
    "dtypeName",
    "sensIfacep",
    "hasUserInit",
    "isLatched",
    "noReset",
    "isPullup",
    "isPulldown",
    "isUsedClock",
    "attrScBv",
    "isReadByDpi",
    "isWrittenByDpi",
    // Constant-folded value of an output driven by a constant; not part of the type.
    "valuep",
];

/// `Some(name)` unless Verilator left the type anonymous (`""`).
fn type_name(name: &str) -> Option<String> {
    let last = name.rsplit(['.', ':']).next().unwrap_or(name);
    (!name.is_empty() && !is_synthetic(last)).then(|| name.to_owned())
}

/// Whether a typedef name was made up by Verilator for an anonymous type
/// (`__typeimpmod1`, `__typeimpsu0`, `__typeimpenum0`; `V3LinkParse.cpp`, `verilog.y`).
fn is_synthetic(name: &str) -> bool {
    name.starts_with("__typeimp")
}

/// Inverts Verilator's `AstNode::encodeName`: `__0XX` is the byte with hex value `XX`.
pub(crate) fn decode_name(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 3..i + 5)
            .filter(|_| bytes[i..].starts_with(b"__0"))
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(b) = hex {
            out.push(b);
            i += 5;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The C++ member name Verilator's `V3Name` pass gives a top-level port: the encoded name,
/// prefixed with `__SYM__` when it is a reserved word (`V3LanguageWords`).
pub(crate) fn cpp_member(encoded: &str) -> String {
    if CPP_RESERVED.contains(&encoded) {
        format!("__SYM__{encoded}")
    } else {
        encoded.to_owned()
    }
}

/// Words `V3LanguageWords` reserves (C++ keywords and common library names), from
/// Verilator 5.053's `src/V3LanguageWords.h`.
const CPP_RESERVED: &[&str] = &[
    "abort",
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "atomic_cancel",
    "atomic_commit",
    "atomic_noexcept",
    "auto",
    "bit_vector",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "cdecl",
    "char",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "complex",
    "concept",
    "const",
    "const_cast",
    "const_iterator",
    "constexpr",
    "continue",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "far",
    "float",
    "for",
    "friend",
    "goto",
    "huge",
    "if",
    "import",
    "inline",
    "int",
    "interrupt",
    "iterator",
    "long",
    "module",
    "mutable",
    "namespace",
    "near",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "override",
    "pascal",
    "private",
    "protected",
    "public",
    "queue",
    "reference",
    "register",
    "requires",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "stack",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "synchronized",
    "template",
    "this",
    "thread_local",
    "throw",
    "transaction_safe",
    "transaction_safe_dynamic",
    "true",
    "try",
    "type_info",
    "typedef",
    "typeid",
    "typename",
    "uint16_t",
    "uint32_t",
    "uint8_t",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "xor",
    "xor_eq",
    "sc_clock",
    "sc_in",
    "sc_inout",
    "sc_out",
    "sc_signal",
    "sensitive",
    "sensitive_neg",
    "sensitive_pos",
];

/// Verilator prints a string constant as `\"text\"` with `\"` and `\\` escaped.
fn unquote(text: &str) -> Option<String> {
    let inner = text.strip_prefix("\\\"")?.strip_suffix("\\\"")?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                c @ ('"' | '\\') => out.push(c),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_verilator_names() {
        assert_eq!(decode_name("a__024b"), "a$b");
        assert_eq!(decode_name("a___05Fb"), "a__b");
        assert_eq!(decode_name("__024unit"), "$unit");
        assert_eq!(decode_name("plain_name"), "plain_name");
        assert_eq!(decode_name("x__0"), "x__0");
        assert_eq!(decode_name("x__0zz"), "x__0zz");
    }

    #[test]
    fn cpp_member_avoids_reserved_words() {
        assert_eq!(cpp_member("char"), "__SYM__char");
        assert_eq!(cpp_member("friend"), "__SYM__friend");
        assert_eq!(cpp_member("a__024b"), "a__024b");
        assert_eq!(cpp_member("data"), "data");
    }

    #[test]
    fn recognises_synthetic_names() {
        assert_eq!(type_name("pkg::state_t").as_deref(), Some("pkg::state_t"));
        assert_eq!(type_name("top.__typeimpmod1"), None);
        assert_eq!(type_name("pkg::__typeimpenum3"), None);
        assert_eq!(type_name(""), None);
    }

    #[test]
    fn unquotes_string_constants() {
        assert_eq!(unquote(r#"\"hi\""#).as_deref(), Some("hi"));
        assert_eq!(unquote(r#"\"a\\b\""#).as_deref(), Some("a\\b"));
        assert_eq!(unquote("hi"), None);
        assert_eq!(unquote(r#"\"a\nb\""#), None);
    }
}
