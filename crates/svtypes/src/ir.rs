//! The versioned, producer-agnostic type IR of an elaborated design.
//!
//! A [`Design`] is plain data: every type lives once in the [`Design::types`] arena and is
//! referred to by [`TypeId`]; every module or interface specialisation lives in
//! [`Design::modules`] and is referred to by [`ModuleId`].
//!
//! Invariants that a front end must establish (and that consumers may rely on):
//! - Type ids are topologically ordered: a type only refers to types with smaller ids.
//! - Identical anonymous types are interned once; named types are distinct by name.
//! - Every packed width was computed from the type structure, never copied from the
//!   producer. [`Type::width`] is `Some` exactly for packed types.
//! - Packed struct field offsets follow IEEE 1800-2023 §7.2.1: the first field is the most
//!   significant.
//! - The schema is versioned by [`SCHEMA`]. Any change that alters the JSON form bumps it.

use serde::{Deserialize, Serialize};

use crate::literal::Literal;

/// Version of the serialized IR. Bumped on every change to the JSON form.
pub const SCHEMA: u32 = 1;

/// An elaborated design: types, modules and packages, as seen by one producer run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Design {
    /// IR schema version; equals [`SCHEMA`] for designs built by this crate.
    pub schema: u32,
    /// The tool that elaborated the design.
    pub producer: Producer,
    /// The global simulation time precision (the finest `timeprecision` in the design).
    pub time_precision: TimeExp,
    /// The top module.
    pub top: ModuleId,
    /// Type arena, indexed by [`TypeId`].
    pub types: Vec<Type>,
    /// Module and interface specialisations, indexed by [`ModuleId`], sorted by
    /// (`orig_name`, `name`).
    pub modules: Vec<Module>,
    /// Packages, sorted by name.
    pub packages: Vec<Package>,
    /// DPI-C imports declared anywhere in the design, sorted by C name, one per C name.
    /// Omitted from the JSON form when empty, so designs without DPI serialize as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dpi_imports: Vec<DpiImport>,
    /// C names of the design's DPI-C exports (SV functions and tasks C may call), sorted.
    /// Omitted from the JSON form when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dpi_exports: Vec<String>,
}

impl Design {
    /// The type with id `id`.
    ///
    /// # Panics
    /// Panics if `id` does not belong to this design.
    #[must_use]
    pub fn ty(&self, id: TypeId) -> &Type {
        &self.types[id.index()]
    }

    /// The module with id `id`.
    ///
    /// # Panics
    /// Panics if `id` does not belong to this design.
    #[must_use]
    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id.index()]
    }

    /// The top module.
    #[must_use]
    pub fn top_module(&self) -> &Module {
        self.module(self.top)
    }
}

/// The elaborating tool and its version, e.g. `verilator` / `5.052`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Producer {
    /// Tool name, lower case.
    pub tool: String,
    /// Tool version as `major.minor` (Verilator style: `5.052`).
    pub version: String,
}

/// A time quantum as a power of ten seconds: `1ns` is `-9`, `10ps` is `-11`.
///
/// IEEE 1800-2023 §3.14.2 only allows magnitudes 1, 10 and 100, so every legal time unit or
/// precision is exactly one power of ten.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimeExp(pub i8);

const TIME_UNITS: [(&str, i8); 6] = [
    ("s", 0),
    ("ms", -3),
    ("us", -6),
    ("ns", -9),
    ("ps", -12),
    ("fs", -15),
];

impl std::str::FromStr for TimeExp {
    type Err = String;

    /// Parses `1ns`, `10ps`, `100fs`, ... (IEEE 1800-2023 §3.14.2.1).
    fn from_str(text: &str) -> Result<Self, String> {
        let split = text
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| format!("time `{text}` has no unit"))?;
        let (magnitude, unit) = text.split_at(split);
        let shift = match magnitude {
            "1" => 0,
            "10" => 1,
            "100" => 2,
            _ => return Err(format!("time `{text}`: magnitude must be 1, 10 or 100")),
        };
        let (_, exp) = TIME_UNITS
            .iter()
            .find(|(u, _)| *u == unit)
            .ok_or_else(|| format!("time `{text}`: unknown unit `{unit}`"))?;
        Ok(TimeExp(exp + shift))
    }
}

impl std::fmt::Display for TimeExp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let base = self.0.div_euclid(3) * 3;
        let magnitude = 10u32.pow(u32::from(self.0.rem_euclid(3).unsigned_abs()));
        match TIME_UNITS.iter().find(|(_, e)| *e == base) {
            Some((unit, _)) => write!(f, "{magnitude}{unit}"),
            None => write!(f, "1e{}s", self.0),
        }
    }
}

/// Index of a [`Type`] in [`Design::types`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TypeId(pub u32);

impl TypeId {
    /// The arena index.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of a [`Module`] in [`Design::modules`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModuleId(pub u32);

impl ModuleId {
    /// The arena index.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A data type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Type {
    /// Qualified SystemVerilog name (`pkg::state_t`, `$unit::t`, `mod::t`); `None` for
    /// anonymous types such as `logic [7:0]`.
    pub name: Option<String>,
    /// For a typedef of another type, the type it renames (one step of the chain). The
    /// `kind` is then a copy of that type's kind, so consumers never chase aliases.
    pub alias_of: Option<TypeId>,
    /// Whether any bit can hold X or Z (IEEE 1800-2023 §6.11: `logic`, `integer`, `time`).
    pub four_state: bool,
    /// Packed width in bits; `None` for unpacked types.
    pub width: Option<u32>,
    /// The structure of the type.
    pub kind: TypeKind,
}

/// The structure of a [`Type`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TypeKind {
    /// An integral vector (`logic [7:0]`, `int`, `bit`).
    Bits {
        /// Width in bits, at least 1.
        width: u32,
        /// Two's complement signed (IEEE 1800-2023 §6.11.3).
        signed: bool,
        /// The declared range, e.g. `[7:0]` or `[0:7]`; `None` for a 1-bit scalar.
        range: Option<Range>,
    },
    /// A struct; packed or unpacked.
    Struct {
        /// Packed (IEEE 1800-2023 §7.2.1).
        packed: bool,
        /// Signed as a whole (`struct packed signed`).
        signed: bool,
        /// Fields in declaration order (most significant first when packed).
        fields: Vec<Field>,
    },
    /// A union; packed or unpacked.
    Union {
        /// Packed (IEEE 1800-2023 §7.3.1). All fields of a packed union have equal width.
        packed: bool,
        /// Signed as a whole (`union packed signed`).
        signed: bool,
        /// Fields in declaration order.
        fields: Vec<Field>,
    },
    /// An enum (IEEE 1800-2023 §6.19).
    Enum {
        /// The base type, an integral [`TypeKind::Bits`] (default `int`).
        base: TypeId,
        /// Named values in declaration order, each as wide and as signed as the base.
        variants: Vec<Variant>,
    },
    /// A packed array `elem [left:right]` (IEEE 1800-2023 §7.4.1).
    PackedArray {
        /// Element type; packed.
        elem: TypeId,
        /// The declared range.
        range: Range,
        /// Signed as a whole (`logic signed [1:0][3:0]`).
        signed: bool,
    },
    /// An unpacked fixed-size array `elem name [left:right]` (IEEE 1800-2023 §7.4.2).
    UnpackedArray {
        /// Element type; packed or unpacked.
        elem: TypeId,
        /// The declared range (`[N]` is `[0:N-1]`).
        range: Range,
    },
}

/// A declared range `[left:right]`. Its width is `|left - right| + 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Range {
    /// The left bound, as written.
    pub left: i32,
    /// The right bound, as written.
    pub right: i32,
}

/// A struct or union field.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Field {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: TypeId,
    /// Bit offset of the field's least significant bit inside a packed aggregate; `None`
    /// when the aggregate is unpacked.
    pub lsb: Option<u32>,
}

/// An enum variant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Variant {
    /// Variant name.
    pub name: String,
    /// Value, as wide and as signed as the enum's base type.
    pub value: Literal,
}

/// Whether a design unit is a module or an interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ModuleKind {
    /// `module`.
    Module,
    /// `interface`.
    Interface,
}

/// One specialisation of a module or interface.
///
/// Parameterised units are elaborated once per distinct parameter set, so several entries
/// may share an `orig_name`; they differ in `params`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    /// Unique name of this specialisation. Equal to `orig_name` when the unit has a single
    /// specialisation; otherwise a producer-chosen mangling (Verilator: `sub__N3`). Do not
    /// parse it: key on `orig_name` plus `params`.
    pub name: String,
    /// The name as declared in the source.
    pub orig_name: String,
    /// Module or interface.
    pub kind: ModuleKind,
    /// Overridable parameters (`parameter`, not `localparam`) with their elaborated values.
    pub params: Vec<Param>,
    /// Data ports in declaration order.
    pub ports: Vec<Port>,
    /// Interface ports in declaration order.
    pub iface_ports: Vec<IfacePort>,
    /// Modports, for an interface.
    pub modports: Vec<Modport>,
}

/// A package with its parameters and named types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Package name (`$unit` for the compilation-unit scope).
    pub name: String,
    /// Parameters and localparams in declaration order.
    pub params: Vec<Param>,
    /// Types declared with `typedef`, in declaration order.
    pub typedefs: Vec<TypeId>,
}

/// An elaborated parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    /// Parameter name.
    pub name: String,
    /// Elaborated value.
    pub value: ParamValue,
    /// Declared type; `None` for `string` and `real` parameters.
    pub ty: Option<TypeId>,
}

/// The value of a parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamValue {
    /// An integral value, as wide and as signed as the parameter's type.
    Bits(Literal),
    /// A `string` value.
    String(String),
    /// A `real` value, as the decimal text the producer printed.
    Real(String),
}

/// Port direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// `input`.
    Input,
    /// `output`.
    Output,
    /// `inout`.
    Inout,
}

/// A data port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    /// Port name as written in SystemVerilog (escaped identifiers without the `\`).
    pub name: String,
    /// Direction.
    pub dir: Direction,
    /// Port type.
    pub ty: TypeId,
    /// Name of the C++ member of the generated model class; `Some` only on the top module,
    /// whose ports are the only ones the model exposes.
    pub cpp_member: Option<String>,
    /// How the producer's C++ model stores a value of this port's type.
    pub storage: Storage,
}

/// The C++ storage Verilator uses for a value (`verilated.h` typedefs).
///
/// Thresholds, checked against generated `V*.h` headers of Verilator 5.052: `CData` up to 8
/// bits, `SData` up to 16, `IData` up to 32, `QData` up to 64, `VlWide<words>` above, with
/// 32-bit words. Packed structs, unions and packed arrays are stored by total width.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Storage {
    /// `uint8_t`, 1 to 8 bits.
    CData,
    /// `uint16_t`, 9 to 16 bits.
    SData,
    /// `uint32_t`, 17 to 32 bits.
    IData,
    /// `uint64_t`, 33 to 64 bits.
    QData,
    /// `VlWide<words>`: `words` 32-bit words, least significant first; above 64 bits.
    VlWide {
        /// Number of 32-bit words, `ceil(width / 32)`.
        words: u32,
    },
    /// `VlUnpacked<elem, len>`: an unpacked array.
    VlUnpacked {
        /// Storage of one element.
        elem: Box<Storage>,
        /// Number of elements.
        len: u32,
    },
}

/// A port whose type is an interface, optionally restricted to a modport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IfacePort {
    /// Port name.
    pub name: String,
    /// The interface specialisation connected to this port.
    pub interface: ModuleId,
    /// The modport named in the port declaration; `None` for a generic interface port.
    pub modport: Option<String>,
}

/// A modport of an interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modport {
    /// Modport name.
    pub name: String,
    /// Signals visible through the modport, in declaration order.
    pub members: Vec<ModportMember>,
}

/// One signal of a modport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModportMember {
    /// Name of the interface signal.
    pub name: String,
    /// Direction as seen from the module using the modport.
    pub dir: Direction,
    /// Signal type.
    pub ty: TypeId,
}

/// A DPI-C import (IEEE 1800-2023 §35.5.4): a function or task the design calls and C
/// (here: Rust) implements. Its C prototype follows from [`DpiImport::result`] and the
/// argument types (Annex H, and Verilator's `V<prefix>__Dpi.h`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiImport {
    /// The SystemVerilog name the design calls.
    pub name: String,
    /// The C symbol: the alias in `import "DPI-C" c_name = function ...`, else `name`.
    pub c_name: String,
    /// The result of a function; `None` for a task or a `void` function. Verilator 5.052's
    /// JSON does not tell those two apart; the C side of a task returns `int` (non-zero
    /// means "disabled", §35.9) and Verilator ignores it, so `int` serves both.
    pub result: Option<DpiType>,
    /// Formal arguments in declaration order.
    pub args: Vec<DpiArg>,
    /// Where the import is declared, `file:line` (file name without directories).
    pub loc: Option<String>,
}

/// A formal argument of a [`DpiImport`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiArg {
    /// Argument name.
    pub name: String,
    /// Direction: `input` is passed by value (or `const` pointer for vectors), `output` and
    /// `inout` by pointer (IEEE 1800-2023 §H.8).
    pub dir: Direction,
    /// Argument type.
    pub ty: DpiType,
}

/// The type of a DPI-C argument or result, as far as it decides the C ABI
/// (IEEE 1800-2023 §H.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DpiType {
    /// `bit`: C `svBit` (`unsigned char`).
    Bit,
    /// `logic` (or `reg`): C `svLogic` (`unsigned char`).
    Logic,
    /// `bit [N-1:0]`: C `svBitVecVal*` (one `uint32_t` per 32 bits); returned by value as
    /// one `svBitVecVal` when at most 32 bits.
    BitVector {
        /// Width in bits.
        width: u32,
        /// Declared `signed`.
        signed: bool,
    },
    /// `logic [N-1:0]`: C `svLogicVecVal*` (an `{aval, bval}` pair per 32 bits).
    LogicVector {
        /// Width in bits.
        width: u32,
        /// Declared `signed`.
        signed: bool,
    },
    /// `byte`: C `char`, or `unsigned char` when declared `unsigned`.
    Byte {
        /// Signed unless declared `byte unsigned`.
        signed: bool,
    },
    /// `shortint`: C `short`.
    ShortInt {
        /// Signed unless declared `shortint unsigned`.
        signed: bool,
    },
    /// `int`: C `int`.
    Int {
        /// Signed unless declared `int unsigned`.
        signed: bool,
    },
    /// `longint`: C `long long`.
    LongInt {
        /// Signed unless declared `longint unsigned`.
        signed: bool,
    },
    /// `chandle`: C `void*`.
    Chandle,
    /// Any other type (`string`, `real`, structs, enums, arrays), described in
    /// SystemVerilog terms; back ends decide whether to reject it.
    Unsupported(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_exp_round_trips() {
        for (text, exp) in [
            ("1s", 0),
            ("100ms", -1),
            ("1ns", -9),
            ("10ps", -11),
            ("100fs", -13),
            ("1fs", -15),
        ] {
            let t: TimeExp = text.parse().unwrap();
            assert_eq!(t, TimeExp(exp), "{text}");
            assert_eq!(t.to_string(), text);
        }
        for bad in ["ns", "2ns", "1xs", "10"] {
            assert!(bad.parse::<TimeExp>().is_err(), "{bad}");
        }
    }
}
