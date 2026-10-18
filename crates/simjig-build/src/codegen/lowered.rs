//! The lowered design: every decision the emitters need, made once.
//!
//! A [`Lowered`] is plain data. Rust names are final (unique, legal, keyword-safe), slots
//! are assigned, storage and access are decided, and SystemVerilog spellings for the
//! layout check and the wrapper are resolved. Emitters only format it, so the Rust code,
//! the `.vlt`, the layout check, the wrapper and the C++ shim cannot disagree.

use svtypes::ir::{Direction, Storage};

/// The output of [`lower`](super::lower).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lowered {
    /// The model's top module, as declared (the wrapper for an interface-ported DUT).
    pub top: String,
    /// The elaborating tool, e.g. `verilator 5.052`.
    pub producer: String,
    /// The first 64 bits of a SHA-256 of the IR (as JSON) and the configuration
    /// (`hash::ir_hash`): the value the C++ shim and the Rust `Dut` both carry, so a model
    /// built from another IR is rejected at load.
    pub ir_hash: u64,
    /// Generated Rust types, in IR type order (dependencies first).
    pub items: Vec<Item>,
    /// Package parameters, one Rust module per package that has any.
    pub packages: Vec<ConstGroup>,
    /// The DUT module's parameters (not the wrapper's), as associated consts of `Dut`.
    pub dut_params: Vec<Const>,
    /// Every signal the testbench can reach; `signals[i].slot == i`.
    pub signals: Vec<Signal>,
    /// The fields of `Dut`, in declaration order.
    pub fields: Vec<DutField>,
    /// Interface bundles referenced by [`DutField::Bundle`].
    pub bundles: Vec<Bundle>,
    /// The elaboration-time layout check.
    pub layout: Layout,
    /// The flat-port wrapper, for an interface-ported DUT.
    pub wrapper: Option<Wrapper>,
}

/// A Rust type expression, rendered by the Rust emitter.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RustType {
    /// `simjig_bits::Bit`, for a 1-bit value.
    Bit,
    /// `simjig_bits::Bits<N>`, 2 to 128 bits.
    Bits(u32),
    /// `simjig_bits::WideBits<N, W>`, above 128 bits.
    WideBits {
        /// Width in bits.
        width: u32,
        /// 32-bit words, `width.div_ceil(32)`.
        words: u32,
    },
    /// A generated type.
    Named(String),
    /// An unpacked array `[T; N]` (Verilator `VlUnpacked`).
    Array(Box<RustType>, u32),
}

/// A generated Rust type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Rust name.
    pub name: String,
    /// SystemVerilog name (`pkg::beat_t`), or where an anonymous type was first used
    /// (`top.in_anon`).
    pub sv_name: String,
    /// Packed width in bits (`$bits`).
    pub width: u32,
    /// Verilator storage of a value of this type; `None` for an alias of an unpacked array.
    pub storage: Option<Storage>,
    /// The type's shape.
    pub kind: ItemKind,
}

/// The shape of an [`Item`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemKind {
    /// A typedef of another type: `pub type Name = Target;`.
    Alias {
        /// The renamed type.
        target: RustType,
    },
    /// A packed struct.
    Struct {
        /// Fields in declaration order (most significant first).
        fields: Vec<StructField>,
    },
    /// A packed union: a newtype over its bits with one accessor per member.
    Union {
        /// The bits every view shares.
        bits: RustType,
        /// Member views in declaration order.
        views: Vec<UnionView>,
    },
    /// An enum: a newtype over the base bits with one const per variant.
    Enum {
        /// The base type's bits ([`RustType::Bit`] or [`RustType::Bits`]).
        base: RustType,
        /// Name of the companion Rust `enum` returned by `variant()`.
        kind_name: String,
        /// Variants in declaration order.
        variants: Vec<EnumVariant>,
    },
}

/// A packed struct field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructField {
    /// Rust field name.
    pub name: String,
    /// SystemVerilog member name.
    pub sv_name: String,
    /// Field type.
    pub ty: RustType,
    /// Offset of the field's least significant bit (IEEE 1800-2023 §7.2.1).
    pub lsb: u32,
    /// Field width in bits.
    pub width: u32,
    /// The field is itself a packed struct, so `diff` recurses into it.
    pub is_struct: bool,
}

/// One member view of a packed union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnionView {
    /// Getter method name.
    pub getter: String,
    /// Constructor name (`from_<getter>`).
    pub constructor: String,
    /// SystemVerilog member name.
    pub sv_name: String,
    /// The member's type.
    pub ty: RustType,
}

/// One enum variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumVariant {
    /// SystemVerilog name.
    pub sv_name: String,
    /// Associated const name on the newtype.
    pub const_name: String,
    /// Variant name in the companion Rust `enum`.
    pub kind_name: String,
    /// Value (enums above 128 bits are unsupported).
    pub value: u128,
}

/// The parameters of one package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstGroup {
    /// Rust module name.
    pub module: String,
    /// SystemVerilog package name.
    pub package: String,
    /// Parameters and localparams in declaration order.
    pub consts: Vec<Const>,
}

/// A parameter as a Rust `const`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Const {
    /// Rust name.
    pub name: String,
    /// SystemVerilog name.
    pub sv_name: String,
    /// Type and value.
    pub value: ConstValue,
}

/// The type and value of a [`Const`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConstValue {
    /// A Rust primitive (`bool`, `u8` .. `u128`, `i8` .. `i128`) and its decimal value.
    Int {
        /// The primitive type.
        ty: &'static str,
        /// The value as a Rust literal.
        value: String,
    },
    /// An unsigned value above 128 bits.
    Wide {
        /// Width in bits.
        width: u32,
        /// Little-endian 32-bit words.
        words: Vec<u32>,
    },
    /// A value of a generated enum.
    Enum {
        /// The enum's Rust name.
        ty: String,
        /// Its base bits.
        base: RustType,
        /// The value.
        value: u128,
    },
    /// A `string` parameter.
    Str(String),
    /// A `real` parameter, as a Rust `f64` literal.
    Real(String),
}

/// How the testbench may access a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Access {
    /// A top input: the testbench writes (and reads back).
    In,
    /// A top output: the testbench reads.
    Out,
    /// A top inout.
    InOut,
    /// A `public_flat_rd` internal signal.
    PublicRd,
    /// A `public_flat_rw` internal signal.
    PublicRw,
}

impl Access {
    /// The access for a top port of direction `dir`.
    #[must_use]
    pub fn from_direction(dir: Direction) -> Self {
        match dir {
            Direction::Input => Self::In,
            Direction::Output => Self::Out,
            Direction::Inout => Self::InOut,
        }
    }
}

/// One signal of the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    /// Index in `Dut::SIGNALS` and in the C++ signal table.
    pub slot: u32,
    /// Hierarchical name: `<top>.<port>` or the probe path.
    pub name: String,
    /// Where the C++ shim finds the member.
    pub target: SignalTarget,
    /// Rust value type.
    pub ty: RustType,
    /// Packed width of one element.
    pub width: u32,
    /// Verilator storage.
    pub storage: Storage,
    /// Access kind.
    pub access: Access,
}

/// Where a signal lives in the Verilated model.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignalTarget {
    /// A member of the model class (`m.<cpp_member>`).
    Port {
        /// The C++ member name.
        cpp_member: String,
    },
    /// A `public_flat` variable, found at create time in Verilator's scope table
    /// (`<model name>.<scope>`, variable `<var>`), wherever Verilator placed it: in the root
    /// when the module was inlined, or in the module's own object when it was not.
    Probe {
        /// The declaring module, for the `.vlt` line.
        module: String,
        /// The variable name.
        var: String,
        /// The elaborated instance path below the model, as Verilator names the scope
        /// (`top.u_core`, or `<wrapper>.dut.u_core` for an interface-ported DUT).
        scope: String,
    },
}

/// A field of the generated `Dut`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DutField {
    /// One signal.
    Signal {
        /// Rust field name.
        name: String,
        /// The signal's slot.
        slot: u32,
    },
    /// An interface bundle over consecutive slots.
    Bundle {
        /// Rust field name.
        name: String,
        /// Index in [`Lowered::bundles`].
        bundle: usize,
        /// Slot of the bundle's first member; member `i` has slot `first_slot + i`.
        first_slot: u32,
    },
}

/// The signals of one interface modport, as the testbench sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    /// Rust type name.
    pub name: String,
    /// `interface.modport`.
    pub sv_name: String,
    /// Members in slot order.
    pub members: Vec<BundleMember>,
}

/// One member of a [`Bundle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleMember {
    /// Rust field name.
    pub name: String,
    /// SystemVerilog signal name inside the interface.
    pub sv_name: String,
    /// Value type.
    pub ty: RustType,
    /// Access kind (from the modport direction, which is the DUT's view: an `input` of the
    /// DUT is driven by the testbench).
    pub access: Access,
}

/// The elaboration-time layout check module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Module name (also the file stem).
    pub module: String,
    /// Local typedefs naming every spelled-out aggregate, dependencies first.
    pub typedefs: Vec<LayoutTypedef>,
    /// One check per generated type.
    pub checks: Vec<LayoutCheck>,
}

/// A typedef declared in the layout-check module for an anonymous or module-local
/// aggregate, which cannot be named from outside the design.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutTypedef {
    /// The typedef's name (`simjig_t<type id>`).
    pub name: String,
    /// The type it declares (`struct packed { .. }`), naming nested aggregates by their
    /// typedefs.
    pub decl: String,
}

/// What the layout check asserts about one type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutCheck {
    /// The type as the messages name it: its SystemVerilog name, or where an anonymous
    /// type was first used (`top.in_anon`).
    pub sv_name: String,
    /// The type, spelled in SystemVerilog: a nameable type's name, a [`LayoutTypedef`]
    /// name, or a vector.
    pub sv_type: String,
    /// `$bits` per the IR.
    pub width: u32,
    /// Member offsets per the IR (union members all have `lsb` 0).
    pub members: Vec<LayoutMember>,
    /// Enum constants per the IR, for enums whose constants are nameable.
    pub values: Vec<LayoutValue>,
}

/// One member of a [`LayoutCheck`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutMember {
    /// Member name.
    pub sv_name: String,
    /// Offset of its least significant bit.
    pub lsb: u32,
    /// Width.
    pub width: u32,
}

/// An enum constant of a [`LayoutCheck`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutValue {
    /// The constant as a SystemVerilog expression (`pkg::BUSY`).
    pub sv_expr: String,
    /// Its value as a sized literal (`2'h1`).
    pub literal: String,
}

/// The generated top that flattens a DUT's interface ports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wrapper {
    /// The wrapper module's name.
    pub module: String,
    /// The DUT module.
    pub dut: String,
    /// DUT parameter overrides.
    pub dut_params: Vec<(String, String)>,
    /// Flat ports: the DUT's data ports, then each interface's signals.
    pub ports: Vec<WrapperPort>,
    /// DUT data-port connections `(port, wrapper port)`.
    pub dut_conns: Vec<(String, String)>,
    /// The interface instances, one per DUT interface port.
    pub ifaces: Vec<WrapperIface>,
}

/// A flat wrapper port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapperPort {
    /// Port name.
    pub name: String,
    /// Direction.
    pub dir: Direction,
    /// The packed type, spelled in SystemVerilog.
    pub packed: String,
    /// Unpacked dimensions after the name (`[0:3]`), or empty.
    pub unpacked: String,
}

/// One interface instance of a [`Wrapper`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapperIface {
    /// Instance name (the DUT's port name).
    pub instance: String,
    /// The interface.
    pub interface: String,
    /// The modport the testbench sees.
    pub modport: String,
    /// Interface parameter overrides.
    pub params: Vec<(String, String)>,
    /// Interface-port connections `(interface port, wrapper port)`.
    pub port_conns: Vec<(String, String)>,
    /// Continuous assignments `(lhs, rhs)`, e.g. `("in_bus.valid", "in_bus_valid")`.
    pub assigns: Vec<(String, String)>,
    /// The bundle members in order: `(signal name, wrapper port, direction)`.
    pub members: Vec<(String, String, Direction)>,
}

/// A parameter sweep: several configurations (members) of one DUT module, lowered together
/// by [`lower_sweep`](super::lower_sweep) into one Rust module that holds what they share
/// and a trait every member's `Dut` implements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweredSweep {
    /// The sweep's Rust module name (`Sweep::new`), which `include_dut!` names.
    pub name: String,
    /// The DUT module every member configures (the user's, not a generated wrapper).
    pub dut: String,
    /// The members, in sweep order: each one's module name and parameters.
    pub members: Vec<SweepMember>,
    /// The generated types identical in every member, defined once in the sweep module
    /// (member 0's IR order, so dependencies come first).
    pub items: Vec<Item>,
    /// The package-constant modules identical in every member.
    pub packages: Vec<ConstGroup>,
    /// The interface bundles identical in every member.
    pub bundles: Vec<Bundle>,
    /// The names each member module imports from the sweep module instead of defining
    /// them: the shared items (and enums' variant enums), packages and bundles.
    pub reexports: Vec<String>,
    /// The trait every member's `Dut` implements.
    pub ports: PortsTrait,
    /// One trait per interface port whose bundle differs between members.
    pub bundle_traits: Vec<PortsTrait>,
    /// One trait per packed struct that is an associated type, over its fields.
    pub fields_traits: Vec<FieldsTrait>,
    /// The runtime traits the sweep module re-exports (`Value`, `Vector`, `Packed`), so a
    /// generic body's one `use <sweep>::*;` brings every bound's methods into scope; less
    /// any a shared type of the sweep is named like.
    pub runtime_traits: Vec<String>,
}

/// A trait over a packed struct whose field types differ between members (`BeatFields`
/// for `beat_t`): a getter per field, a constructor `new` and a `with_<field>` per field,
/// so a generic body can read and build the struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldsTrait {
    /// Rust name.
    pub name: String,
    /// The struct's SystemVerilog name in the first member.
    pub sv_name: String,
    /// The struct in each member, as a path from the sweep module.
    pub impls: Vec<String>,
    /// Associated types: one per distinct list of per-member field types.
    pub types: Vec<AssocType>,
    /// The fields, in declaration order (most significant first).
    pub fields: Vec<TraitField>,
}

/// A field of a [`FieldsTrait`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitField {
    /// Rust name, the struct field's.
    pub name: String,
    /// Its getter: the field's name, with `_` appended when the trait or a supertrait
    /// has a method of that name (`new`, `eq`, `to_raw`, ..., or another field's setter).
    pub getter: String,
    /// Its setter, `with_<field>`.
    pub setter: String,
    /// SystemVerilog member name.
    pub sv_name: String,
    /// Its type.
    pub ty: TraitType,
}

/// One configuration of a [`LoweredSweep`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepMember {
    /// Its Rust module name inside the sweep module (the DUT name).
    pub name: String,
    /// The DUT's parameters in this configuration.
    pub params: Vec<Const>,
}

/// A generated trait over the signals several generated types have in common: the
/// sweep's `Ports` over each member's `Dut`, or a bundle trait over interface bundles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortsTrait {
    /// Rust name.
    pub name: String,
    /// What it describes: the DUT module, or `interface.modport`.
    pub sv_name: String,
    /// The implementing types, as paths from the sweep module (`fifo_small::Dut`); every
    /// per-implementation list below is in this order.
    pub impls: Vec<String>,
    /// Parameters every implementation has with one Rust type, as associated consts.
    pub consts: Vec<TraitConst>,
    /// Associated types: one per distinct list of per-implementation types.
    pub types: Vec<AssocType>,
    /// One method per signal or bundle every implementation has with one access.
    pub methods: Vec<TraitMethod>,
    /// Fields left out: not in every implementation, or not alike in all of them.
    pub omitted: Vec<String>,
}

/// An associated const of a [`PortsTrait`]: a DUT parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitConst {
    /// Rust name, as on each `Dut`.
    pub name: String,
    /// SystemVerilog name.
    pub sv_name: String,
    /// The first implementation's value; its type is every implementation's.
    pub value: ConstValue,
}

/// An associated type of a [`PortsTrait`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssocType {
    /// Rust name.
    pub name: String,
    /// What a generic body may assume of it.
    pub bound: AssocBound,
    /// The type in each implementation, with paths from the sweep module.
    pub per_impl: Vec<RustType>,
    /// The fields whose type it is, in declaration order.
    pub users: Vec<String>,
    /// Associated types of the bound (a bundle trait) fixed to this trait's: the bound's
    /// name for one, and the index of this trait's type it equals. So `up.beat` and
    /// `down.beat`, in two bundle traits, are one type in a generic body.
    pub equals: Vec<(String, usize)>,
}

/// The bound of an [`AssocType`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssocBound {
    /// `simjig::Vector`: a bit vector in every implementation.
    Vector,
    /// `simjig::Value`: a packed value in every implementation.
    Value,
    /// No bound: an unpacked array somewhere, which has no `get`/`set`.
    None,
    /// A bundle trait or a [`FieldsTrait`] of the sweep, by name.
    Trait(String),
}

/// A method of a [`PortsTrait`]: returns a field of the implementing type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitMethod {
    /// Rust name: the field's, or the field's with `_` appended when a supertrait has a
    /// method of that name (`clone_` for a port `clone`: `Clone::clone`).
    pub name: String,
    /// The field it returns.
    pub field: String,
    /// The signal's hierarchical name in the first implementation (`fifo.clk`), or the
    /// interface port's `interface.modport`.
    pub sv_name: String,
    /// What it returns.
    pub kind: MethodKind,
}

/// What a [`TraitMethod`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MethodKind {
    /// A signal handle of this access.
    Signal {
        /// The handle kind.
        access: Access,
        /// Its value type.
        ty: TraitType,
    },
    /// An interface bundle.
    Bundle(TraitType),
}

/// A type in a [`PortsTrait`]'s signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraitType {
    /// The same type in every implementation (generated names are the sweep's shared
    /// items).
    Concrete(RustType),
    /// The associated type at this index of [`PortsTrait::types`].
    Assoc(usize),
}
