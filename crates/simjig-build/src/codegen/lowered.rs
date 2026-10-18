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
    /// FNV-1a 64 of the IR (as JSON) and the configuration: the value the C++ shim and
    /// the Rust `Dut` both carry, so a model built from another IR is rejected at load.
    pub ir_hash: u64,
    /// Generated Rust types, in IR type order (dependencies first).
    pub items: Vec<Item>,
    /// Package parameters, one Rust module per package that has any.
    pub packages: Vec<ConstGroup>,
    /// The top module's parameters, as associated consts of `Dut`.
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
pub enum SignalTarget {
    /// A member of the model class (`m.<cpp_member>`).
    Port {
        /// The C++ member name.
        cpp_member: String,
    },
    /// A `public_flat` member of the root class (`m.rootp-><root_member>`).
    Probe {
        /// The declaring module, for the `.vlt` line.
        module: String,
        /// The variable name.
        var: String,
        /// The encoded root member (`top__DOT__u__DOT__x`).
        root_member: String,
    },
}

/// A field of the generated `Dut`.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// One check per generated type.
    pub checks: Vec<LayoutCheck>,
}

/// What the layout check asserts about one type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutCheck {
    /// The type, spelled in SystemVerilog (a name, or the structure of an anonymous or
    /// module-local type).
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
