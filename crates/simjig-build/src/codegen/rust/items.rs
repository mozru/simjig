//! Generated types: packed structs, packed unions, enums and aliases.
//!
//! Every generated type implements `Packed` (simjig-bits) and keeps its contract: the raw
//! form is Verilator's storage for the type's width, `to_raw` is clean above the width,
//! and `from_raw` is total. Struct fields are placed at their IR offsets through the
//! runtime's `pack_field`/`unpack_field`; a `const` block asserts at compile time that each
//! field type is as wide as the IR says.

use super::super::writer::Writer;
use super::super::{EnumVariant, Item, ItemKind, RustType, StructField, UnionView};
use super::{BITS, RT, base_value, derive, prim, raw_type, render};

/// Emits one item.
pub(crate) fn emit_item(w: &mut Writer, item: &Item) {
    match &item.kind {
        ItemKind::Alias { target } => {
            w.line(format!(
                "/// SystemVerilog `{}` ({} bits).",
                item.sv_name, item.width
            ));
            w.line(format!("pub type {} = {};", item.name, render(target)));
        }
        ItemKind::Struct { fields } => emit_struct(w, item, fields),
        ItemKind::Union { bits, views } => emit_union(w, item, bits, views),
        ItemKind::Enum {
            base,
            kind_name,
            variants,
        } => emit_enum(w, item, base, kind_name, variants),
    }
}

/// `msb:lsb` of a field.
fn bit_range(lsb: u32, width: u32) -> String {
    format!("[{}:{lsb}]", lsb + width - 1)
}

fn raw(item: &Item) -> String {
    item.storage
        .as_ref()
        .map_or_else(|| "()".to_owned(), raw_type)
}

fn emit_packed_header(w: &mut Writer, item: &Item, raw: &str) {
    w.open(format!("impl {BITS}::Packed for {} {{", item.name));
    w.line(format!("const WIDTH: {} = {};", prim("u32"), item.width));
    w.line(format!("type Raw = {raw};"));
    w.line("");
}

fn emit_struct(w: &mut Writer, item: &Item, fields: &[StructField]) {
    let words = item.width.div_ceil(32);
    let raw = raw(item);
    w.line(format!(
        "/// SystemVerilog `{}`: a packed struct of {} bits, first field most significant.",
        item.sv_name, item.width
    ));
    w.line(derive(&[
        "Clone",
        "Copy",
        "PartialEq",
        "Eq",
        "Hash",
        "Default",
        "Debug",
    ]));
    w.open(format!("pub struct {} {{", item.name));
    for f in fields {
        w.line(format!(
            "/// `{}`, bits `{}`.",
            f.sv_name,
            bit_range(f.lsb, f.width)
        ));
        w.line(format!("pub {}: {},", f.name, render(&f.ty)));
    }
    w.close("}");
    w.line("");
    emit_width_asserts(w, item, fields);
    w.line("");
    emit_packed_header(w, item, &raw);
    w.open(format!("fn to_raw(self) -> {raw} {{"));
    w.line(format!(
        "let mut words: [{}; {words}] = [0; {words}];",
        prim("u32")
    ));
    for f in fields {
        w.line(format!(
            "{RT}::pack_field(&mut words, {}, self.{});",
            f.lsb, f.name
        ));
    }
    w.line(format!("{RT}::RawWords::from_words(&words)"));
    w.close("}");
    w.line("");
    w.open(format!("fn from_raw(raw: {raw}) -> Self {{"));
    w.line(format!(
        "let mut words: [{}; {words}] = [0; {words}];",
        prim("u32")
    ));
    w.line(format!("{RT}::RawWords::to_words(raw, &mut words);"));
    w.open("Self {");
    for f in fields {
        w.line(format!(
            "{}: {RT}::unpack_field(&words, {}),",
            f.name, f.lsb
        ));
    }
    w.close("}");
    w.close("}");
    w.close("}");
    w.line("");
    emit_diff(w, item, fields);
}

/// A `const` block asserting that each field type is as wide as the IR says.
fn emit_width_asserts(w: &mut Writer, item: &Item, fields: &[StructField]) {
    w.open("const _: () = {");
    for f in fields {
        w.line(format!(
            "::core::assert!(<{} as {BITS}::Packed>::WIDTH == {}, \"{}.{} must be {} bits wide\");",
            render(&f.ty),
            f.width,
            item.sv_name,
            f.sv_name,
            f.width
        ));
    }
    w.close("};");
}

fn emit_diff(w: &mut Writer, item: &Item, fields: &[StructField]) {
    w.open(format!("impl {} {{", item.name));
    w.line("/// The fields that differ between `self` and `other`, by SystemVerilog name.");
    w.line("/// Nested structs are compared field by field (`outer.inner.tag`).");
    w.line("#[must_use]");
    w.open(format!(
        "pub fn diff(&self, other: &Self) -> ::std::vec::Vec<{RT}::FieldDiff> {{"
    ));
    w.line("let mut diffs = ::std::vec::Vec::new();");
    for f in fields {
        let n = &f.name;
        if f.is_struct {
            w.line(format!(
                "diffs.extend(self.{n}.diff(&other.{n}).into_iter().map(|d| d.within({:?})));",
                f.sv_name
            ));
        } else {
            w.open(format!("if self.{n} != other.{n} {{"));
            w.line(format!(
                "diffs.push({RT}::FieldDiff::new({:?}, &self.{n}, &other.{n}));",
                f.sv_name
            ));
            w.close("}");
        }
    }
    w.line("diffs");
    w.close("}");
    w.close("}");
}

fn emit_union(w: &mut Writer, item: &Item, bits: &RustType, views: &[UnionView]) {
    let (name, bits_ty, raw) = (&item.name, render(bits), raw(item));
    w.line(format!(
        "/// SystemVerilog `{}`: a packed union of {} bits; each member is a view of the same bits.",
        item.sv_name, item.width
    ));
    w.line(derive(&[
        "Clone",
        "Copy",
        "PartialEq",
        "Eq",
        "Hash",
        "Default",
    ]));
    w.line(format!("pub struct {name}({bits_ty});"));
    w.line("");
    w.open(format!("impl {name} {{"));
    w.line("/// Wraps raw bits.");
    w.line(format!(
        "pub const fn from_bits(bits: {bits_ty}) -> Self {{ Self(bits) }}"
    ));
    w.line("");
    w.line("/// The raw bits.");
    w.line(format!(
        "pub const fn to_bits(self) -> {bits_ty} {{ self.0 }}"
    ));
    for v in views {
        let ty = render(&v.ty);
        w.line("");
        w.line(format!("/// The bits viewed as member `{}`.", v.sv_name));
        w.open(format!("pub fn {}(self) -> {ty} {{", v.getter));
        w.line(format!(
            "<{ty} as {BITS}::Packed>::from_raw(<{bits_ty} as {BITS}::Packed>::to_raw(self.0))"
        ));
        w.close("}");
        w.line("");
        w.line(format!(
            "/// The value whose member `{}` is `value`.",
            v.sv_name
        ));
        w.open(format!("pub fn {}(value: {ty}) -> Self {{", v.constructor));
        w.line(format!(
            "Self(<{bits_ty} as {BITS}::Packed>::from_raw(<{ty} as {BITS}::Packed>::to_raw(value)))"
        ));
        w.close("}");
    }
    w.close("}");
    w.line("");
    w.open(format!("impl ::core::fmt::Debug for {name} {{"));
    w.open("fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {");
    w.line(format!("f.debug_struct({name:?})"));
    for v in views {
        w.line(format!("    .field({:?}, &self.{}())", v.sv_name, v.getter));
    }
    w.line("    .finish()");
    w.close("}");
    w.close("}");
    w.line("");
    emit_packed_delegate(w, item, &raw);
}

/// `Packed` for a newtype over bits: delegate to the bits.
fn emit_packed_delegate(w: &mut Writer, item: &Item, raw: &str) {
    emit_packed_header(w, item, raw);
    w.line(format!(
        "fn to_raw(self) -> {raw} {{ {BITS}::Packed::to_raw(self.0) }}"
    ));
    w.line("");
    w.line(format!(
        "fn from_raw(raw: {raw}) -> Self {{ Self({BITS}::Packed::from_raw(raw)) }}"
    ));
    w.close("}");
}

fn emit_enum(w: &mut Writer, item: &Item, base: &RustType, kind: &str, variants: &[EnumVariant]) {
    let (name, base_ty, raw) = (&item.name, render(base), raw(item));
    w.line(format!(
        "/// SystemVerilog enum `{}` ({} bits).",
        item.sv_name, item.width
    ));
    w.line("///");
    w.line(
        "/// An SV enum variable can hold values that name no variant (IEEE 1800-2023 §6.19), so",
    );
    w.line("/// this is a newtype over the bits: reading never fails, and [`Self::variant`] says");
    w.line("/// whether the value is a member. `Default` is the all-zero value.");
    w.line(derive(&[
        "Clone",
        "Copy",
        "PartialEq",
        "Eq",
        "Hash",
        "Default",
    ]));
    w.line(format!("pub struct {name}({base_ty});"));
    w.line("");
    w.open(format!("impl {name} {{"));
    for v in variants {
        w.line(format!("/// `{}`.", v.sv_name));
        w.line(format!(
            "pub const {}: Self = Self({});",
            v.const_name,
            base_value(base, v.value)
        ));
    }
    w.line("");
    w.line("/// Wraps raw bits; values that name no variant are kept.");
    w.line(format!(
        "pub const fn from_bits(bits: {base_ty}) -> Self {{ Self(bits) }}"
    ));
    w.line("");
    w.line("/// The raw bits.");
    w.line(format!(
        "pub const fn to_bits(self) -> {base_ty} {{ self.0 }}"
    ));
    w.line("");
    w.line("/// The variant, or an error carrying the raw value if it names none.");
    w.open(format!(
        "pub fn variant(self) -> ::core::result::Result<{kind}, {RT}::InvalidEnum> {{"
    ));
    // An `if` chain, not a `match`: over `Bit` two variants cover every value, and a
    // match would then have an unreachable fallback arm.
    for (i, v) in variants.iter().enumerate() {
        let keyword = if i == 0 { "let kind = if" } else { "} else if" };
        w.line(format!("{keyword} self == Self::{} {{", v.const_name));
        w.line(format!("    {kind}::{}", v.kind_name));
    }
    let raw_value = if *base == RustType::Bit {
        format!("{}::from(self.0.to_bool())", prim("u128"))
    } else {
        "self.0.to_u128()".to_owned()
    };
    w.open("} else {");
    w.line(format!(
        "return ::core::result::Result::Err({RT}::InvalidEnum::new({:?}, {}, {raw_value}));",
        item.sv_name, item.width
    ));
    w.close("};");
    w.line("::core::result::Result::Ok(kind)");
    w.close("}");
    w.close("}");
    w.line("");
    emit_enum_debug(w, name, kind, variants);
    w.line("");
    emit_packed_delegate(w, item, &raw);
    w.line("");
    emit_enum_kind(w, name, kind, variants);
}

/// The companion Rust `enum` of a generated enum, and its conversion.
fn emit_enum_kind(w: &mut Writer, name: &str, kind: &str, variants: &[EnumVariant]) {
    w.line(format!("/// The variants of [`{name}`]."));
    w.line(derive(&[
        "Clone",
        "Copy",
        "PartialEq",
        "Eq",
        "Hash",
        "Debug",
    ]));
    w.open(format!("pub enum {kind} {{"));
    for v in variants {
        w.line(format!("/// `{}`.", v.sv_name));
        w.line(format!("{},", v.kind_name));
    }
    w.close("}");
    w.line("");
    w.open(format!("impl ::core::convert::From<{kind}> for {name} {{"));
    w.open(format!("fn from(kind: {kind}) -> Self {{"));
    w.open("match kind {");
    for v in variants {
        w.line(format!(
            "{kind}::{} => Self::{},",
            v.kind_name, v.const_name
        ));
    }
    w.close("}");
    w.close("}");
    w.close("}");
}

/// `Debug` of a generated enum: the SV variant name, or the raw bits.
fn emit_enum_debug(w: &mut Writer, name: &str, kind: &str, variants: &[EnumVariant]) {
    w.open(format!("impl ::core::fmt::Debug for {name} {{"));
    w.open("fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {");
    w.open("let name = match self.variant() {");
    for v in variants {
        w.line(format!(
            "::core::result::Result::Ok({kind}::{}) => {:?},",
            v.kind_name, v.sv_name
        ));
    }
    w.line(format!(
        "::core::result::Result::Err(_) => return ::core::write!(f, \"{name}({{:?}})\", self.0),"
    ));
    w.close("};");
    w.line("f.write_str(name)");
    w.close("}");
    w.close("}");
}
