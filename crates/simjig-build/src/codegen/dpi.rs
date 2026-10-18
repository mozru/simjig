//! The DPI-C emitter: Rust implementations of a design's `import "DPI-C"` functions.
//!
//! For a design with imports ([`svtypes::ir::Design::dpi_imports`]) it emits, after the
//! Rust emitter's output:
//! - `pub trait Dpi` (`DutDpi` if the design already names a type `Dpi`), one method per
//!   import: inputs by value, outputs and `inout` arguments as `&mut`, the function
//!   result as the return type;
//! - `Dut::set_dpi(&self, impl Dpi)`, which registers an implementation for this model
//!   instance (`simjig::__private::dpi::install`, found through one of the `Dut`'s
//!   handles);
//! - one `::simjig::__private::dpi_import!` per import: the `extern "C"` trampoline whose
//!   symbol is the import's C name and whose parameters are the Annex H C types (see the
//!   runtime's `dpi::abi`).
//!
//! The trampolines' C prototypes ([`dpi_prototypes`]) are cross-checked against
//! Verilator's `V<prefix>__Dpi.h` after verilation ([`check_dpi_header`]), so the IR's
//! reading of the ABI cannot drift from what the model calls.
//!
//! This emitter reads the IR directly, not the [`Lowered`] form, except for the `Dut`
//! field `set_dpi` uses and the type names it must not collide with; DPI signatures carry
//! no type the lowering names.

use svtypes::ir::{Direction, DpiArg, DpiImport, DpiType};

use super::Error;
use super::lowered::{DutField, Lowered};
use super::names::{Namespace, snake};
use super::rust::{BITS, RT};
use super::writer::Writer;

/// The widest vector argument simjig passes (a `Bits<N>` read from two C words).
const MAX_VECTOR: u32 = 64;

/// Clippy's default `too_many_arguments` threshold, `self` included.
const CLIPPY_MAX_ARGS: usize = 7;

/// What simjig supports, for error messages.
const SUPPORTED: &str = "simjig implements DPI-C arguments and results of type bit, logic, \
                         bit and logic vectors up to 64 bits, byte, shortint, int, longint \
                         (each also unsigned) and chandle";

/// The DPI-C items for the Rust-implemented `imports` (a design's
/// [`dpi_imports`](svtypes::ir::Design::dpi_imports), less those left to C; empty without
/// any); `prefix` is the model's Verilator
/// prefix, which the runtime uses to tell models apart.
///
/// # Errors
/// [`Error::Unsupported`] naming the import for an argument or result type outside
/// what the runtime implements (scalars, integers, vectors up to 64 bits, `chandle`), a C
/// name that is not a usable Rust identifier, or a model without ports.
pub fn emit_dpi(imports: &[DpiImport], lowered: &Lowered, prefix: &str) -> Result<String, Error> {
    if imports.is_empty() {
        return Ok(String::new());
    }
    for import in imports {
        check(import)?;
    }
    let trait_name = trait_name(lowered);
    let handle = any_handle(lowered).ok_or_else(|| {
        Error::unsupported(
            format!("DPI-C imports of `{}`", lowered.top),
            "the model has no ports, so `Dut::set_dpi` has no handle to find its simulation by",
        )
    })?;
    let mut methods = Namespace::default();
    let methods: Vec<Method> = imports
        .iter()
        .map(|i| Method::new(i, &mut methods))
        .collect();
    let mut w = Writer::new("    ");
    w.line("");
    emit_trait(&mut w, &trait_name, &lowered.top, &methods);
    w.line("");
    emit_set_dpi(&mut w, &trait_name, prefix, &handle, imports);
    for m in &methods {
        w.line("");
        emit_trampoline(&mut w, &trait_name, prefix, m);
    }
    Ok(w.finish())
}

/// One import with its Rust names.
struct Method<'a> {
    import: &'a DpiImport,
    /// The trait method.
    name: String,
    /// The C symbol as a Rust identifier.
    symbol: String,
    /// Argument identifiers, in order.
    args: Vec<String>,
}

impl<'a> Method<'a> {
    fn new(import: &'a DpiImport, methods: &mut Namespace) -> Self {
        let mut args = Namespace::with_reserved(&["self", "self_"]);
        Self {
            import,
            name: methods.claim(&snake(&import.name)),
            symbol: symbol(&import.c_name),
            args: import
                .args
                .iter()
                .map(|a| args.claim(&snake(&a.name)))
                .collect(),
        }
    }
}

/// `DPI-C import `f` (dpi.sv:3)`: the item every error and doc line names.
fn item(import: &DpiImport) -> String {
    match &import.loc {
        Some(loc) => format!("DPI-C import `{}` ({loc})", import.name),
        None => format!("DPI-C import `{}`", import.name),
    }
}

/// What a result may be, for error messages (IEEE 1800-2023 §35.5.5).
const RESULTS: &str = "a DPI-C result is bit, logic, byte, shortint, int, longint (each also \
                       unsigned), chandle, or a bit vector of at most 32 bits \
                       (IEEE 1800-2023 §35.5.5)";

/// Rejects what the runtime cannot marshal.
fn check(import: &DpiImport) -> Result<(), Error> {
    for arg in &import.args {
        if !supported(&arg.ty) {
            return Err(Error::unsupported(
                item(import),
                format!(
                    "argument `{}` has {}; {SUPPORTED}",
                    arg.name,
                    describe(&arg.ty)
                ),
            ));
        }
    }
    if let Some(ty) = &import.result {
        // Verilator itself rejects wider or 4-state vector results.
        let returnable = match ty {
            DpiType::BitVector { width, .. } => *width <= 32,
            DpiType::LogicVector { .. } => false,
            other => supported(other),
        };
        if !returnable {
            return Err(Error::unsupported(
                item(import),
                format!("the result has {}; {RESULTS}", describe(ty)),
            ));
        }
    }
    if matches!(
        import.c_name.as_str(),
        "self" | "Self" | "super" | "crate" | "_"
    ) {
        return Err(Error::unsupported(
            item(import),
            format!(
                "the C name `{}` cannot be a Rust function name; give the import a C alias \
                 (`import \"DPI-C\" other_name = function ...`)",
                import.c_name
            ),
        ));
    }
    Ok(())
}

fn supported(ty: &DpiType) -> bool {
    match ty {
        DpiType::BitVector { width, .. } | DpiType::LogicVector { width, .. } => {
            *width <= MAX_VECTOR
        }
        DpiType::Unsupported(_) => false,
        _ => true,
    }
}

/// The type for error messages: ``type `string` ``, ``type `bit [64:0]` (65 bits)``.
fn describe(ty: &DpiType) -> String {
    match ty {
        DpiType::Unsupported(what) => format!("type {what}"),
        DpiType::BitVector { width, .. } | DpiType::LogicVector { width, .. } => {
            format!("type `{}` ({width} bits)", sv_type(ty))
        }
        other => format!("type `{}`", sv_type(other)),
    }
}

/// The C name as an identifier: a raw identifier for a Rust keyword.
fn symbol(c_name: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do",
        "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in",
        "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
        "return", "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe",
        "unsized", "use", "virtual", "where", "while", "yield",
    ];
    if KEYWORDS.contains(&c_name) {
        format!("r#{c_name}")
    } else {
        c_name.to_owned()
    }
}

/// `Dpi`, unless a generated type already has that name.
fn trait_name(lowered: &Lowered) -> String {
    let taken = |n: &str| {
        lowered.items.iter().any(|i| i.name == n) || lowered.bundles.iter().any(|b| b.name == n)
    };
    ["Dpi", "DutDpi", "DutDpiImports"]
        .into_iter()
        .find(|n| !taken(n))
        .unwrap_or("DutDpiImports")
        .to_owned()
}

/// A `Dut` handle expression (`self.clk`), through which `set_dpi` finds the simulation.
fn any_handle(lowered: &Lowered) -> Option<String> {
    lowered.fields.iter().find_map(|f| match f {
        DutField::Signal { name, .. } => Some(format!("self.{name}")),
        DutField::Bundle { name, bundle, .. } => {
            let member = lowered.bundles.get(*bundle)?.members.first()?;
            Some(format!("self.{name}.{}", member.name))
        }
    })
}

/// The SystemVerilog spelling of a type, for docs and errors.
fn sv_type(ty: &DpiType) -> String {
    let unsigned = |base: &str, signed: bool| {
        if signed {
            base.to_owned()
        } else {
            format!("{base} unsigned")
        }
    };
    let vector = |base: &str, width: u32, signed: bool| {
        let sign = if signed { " signed" } else { "" };
        format!("{base}{sign} [{}:0]", width.saturating_sub(1))
    };
    match ty {
        DpiType::Bit => "bit".to_owned(),
        DpiType::Logic => "logic".to_owned(),
        DpiType::BitVector { width, signed } => vector("bit", *width, *signed),
        DpiType::LogicVector { width, signed } => vector("logic", *width, *signed),
        DpiType::Byte { signed } => unsigned("byte", *signed),
        DpiType::ShortInt { signed } => unsigned("shortint", *signed),
        DpiType::Int { signed } => unsigned("int", *signed),
        DpiType::LongInt { signed } => unsigned("longint", *signed),
        DpiType::Chandle => "chandle".to_owned(),
        DpiType::Unsupported(what) => what.clone(),
    }
}

/// `int f(input int x, output bit [7:0] o)`.
fn sv_signature(import: &DpiImport) -> String {
    let args: Vec<String> = import
        .args
        .iter()
        .map(|a| format!("{} {} {}", dir_word(a.dir), sv_type(&a.ty), a.name))
        .collect();
    let result = import
        .result
        .as_ref()
        .map_or_else(|| "void".to_owned(), sv_type);
    format!("{result} {}({})", import.name, args.join(", "))
}

fn dir_word(dir: Direction) -> &'static str {
    match dir {
        Direction::Input => "input",
        Direction::Output => "output",
        Direction::Inout => "inout",
    }
}

/// The Rust value type of a supported type.
fn rust_type(ty: &DpiType) -> String {
    match ty {
        DpiType::Bit | DpiType::Logic => format!("{BITS}::Bit"),
        DpiType::BitVector { width, .. } | DpiType::LogicVector { width, .. } => {
            format!("{BITS}::Bits<{width}>")
        }
        DpiType::Byte { signed } => int_type(8, *signed),
        DpiType::ShortInt { signed } => int_type(16, *signed),
        DpiType::Int { signed } => int_type(32, *signed),
        DpiType::LongInt { signed } => int_type(64, *signed),
        DpiType::Chandle => "::simjig::Chandle".to_owned(),
        DpiType::Unsupported(_) => unreachable!("invariant: `check` rejected unsupported types"),
    }
}

fn int_type(bits: u32, signed: bool) -> String {
    format!("{}{bits}", if signed { 'i' } else { 'u' })
}

/// The runtime's `Abi` marker of a supported type.
fn marker(ty: &DpiType) -> String {
    let m = match ty {
        DpiType::Bit => "Bit".to_owned(),
        DpiType::Logic => "Logic".to_owned(),
        DpiType::BitVector { width, .. } => format!("BitVec<{width}>"),
        DpiType::LogicVector { width, .. } => format!("LogicVec<{width}>"),
        DpiType::Byte { signed } => int_type(8, *signed).to_uppercase(),
        DpiType::ShortInt { signed } => int_type(16, *signed).to_uppercase(),
        DpiType::Int { signed } => int_type(32, *signed).to_uppercase(),
        DpiType::LongInt { signed } => int_type(64, *signed).to_uppercase(),
        DpiType::Chandle => "Handle".to_owned(),
        DpiType::Unsupported(_) => unreachable!("invariant: `check` rejected unsupported types"),
    };
    format!("{RT}::dpi::{m}")
}

fn emit_trait(w: &mut Writer, name: &str, top: &str, methods: &[Method<'_>]) {
    w.line(format!(
        "/// The DPI-C imports of `{top}`, implemented in Rust; register an implementation with"
    ));
    w.line("/// [`Dut::set_dpi`]. Each method runs inside the model's eval, in the middle of a");
    w.line("/// time step: it cannot read or write signals or await, and a panic fails the run.");
    w.line("/// A DPI implementation can't drive another simjig model inside a call.");
    w.open(format!("pub trait {name} {{"));
    for (i, m) in methods.iter().enumerate() {
        if i > 0 {
            w.line("");
        }
        let import = m.import;
        let mut notes: Vec<String> = import.loc.iter().map(|l| format!("`{l}`")).collect();
        if import.c_name != import.name {
            notes.push(format!("C name `{}`", import.c_name));
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join("; "))
        };
        w.line(format!("/// `{}`{notes}.", sv_signature(import)));
        if import.result.is_none() {
            w.line("///");
            w.line("/// A task or a `void` function: it returns nothing.");
        }
        if import.args.len() + 1 > CLIPPY_MAX_ARGS {
            w.line(
                "#[expect(clippy::too_many_arguments, reason = \"one parameter per SV argument\")]",
            );
        }
        let params: Vec<String> = import
            .args
            .iter()
            .zip(&m.args)
            .map(|(a, ident)| format!(", {ident}: {}", param_type(a)))
            .collect();
        let result = import
            .result
            .as_ref()
            .map_or_else(String::new, |t| format!(" -> {}", rust_type(t)));
        w.line(format!(
            "fn {}(&mut self{}){result};",
            m.name,
            params.concat()
        ));
    }
    w.close("}");
}

fn param_type(arg: &DpiArg) -> String {
    match arg.dir {
        Direction::Input => rust_type(&arg.ty),
        Direction::Output | Direction::Inout => format!("&mut {}", rust_type(&arg.ty)),
    }
}

fn emit_set_dpi(w: &mut Writer, name: &str, prefix: &str, handle: &str, imports: &[DpiImport]) {
    let names: Vec<String> = imports.iter().map(|i| format!("{:?}", i.c_name)).collect();
    w.open("impl Dut {");
    w.line(format!(
        "/// Registers `imp` as this model instance's implementation of [`{name}`],"
    ));
    w.line("/// replacing any earlier one. Register it before the first eval: before");
    w.line("/// `Sim::run`, or first thing in the test, before any await.");
    w.line("///");
    w.line("/// # Panics");
    w.line("/// If another model loaded in this process imports one of the same C names");
    w.line("/// (a DPI-C import is one C symbol per binary).");
    w.line("#[track_caller]");
    w.open(format!(
        "pub fn set_dpi(&self, imp: impl {name} + 'static) {{"
    ));
    w.line(format!(
        "let imp: ::std::boxed::Box<dyn {name}> = ::std::boxed::Box::new(imp);"
    ));
    w.line(format!("{RT}::dpi::install::<dyn {name}>("));
    w.line(format!("    {prefix:?},"));
    w.line(format!("    &[{}],", names.join(", ")));
    w.line(format!("    {handle}.raw(),"));
    w.line("    imp,");
    w.line(");");
    w.close("}");
    w.close("}");
}

fn emit_trampoline(w: &mut Writer, name: &str, prefix: &str, m: &Method<'_>) {
    w.open(format!("{RT}::dpi_import! {{"));
    w.line(format!("prefix = {prefix:?}, imp = dyn {name},"));
    let result = m
        .import
        .result
        .as_ref()
        .map_or_else(String::new, |ty| format!(" -> {}", marker(ty)));
    if m.import.args.is_empty() {
        w.line(format!("fn {} => {}(){result}", m.symbol, m.name));
        w.close("}");
        return;
    }
    w.open(format!("fn {} => {}(", m.symbol, m.name));
    for (arg, ident) in m.import.args.iter().zip(&m.args) {
        let wrapper = match arg.dir {
            Direction::Input => "InArg",
            Direction::Output => "OutArg",
            Direction::Inout => "InOutArg",
        };
        w.line(format!("{ident}: {wrapper} {},", marker(&arg.ty)));
    }
    w.close(format!("){result}"));
    w.close("}");
}

/// The C prototype of each import's trampoline, keyed by C name, in Verilator's spelling
/// (`int f_int(int x, int* o, int* io)`; the result of a task or `void` function is shown
/// as `void`, which [`check_dpi_header`] also accepts as `int`).
#[must_use]
pub fn dpi_prototypes(imports: &[DpiImport]) -> Vec<(String, String)> {
    imports
        .iter()
        .map(|i| {
            let params: Vec<String> = i
                .args
                .iter()
                .map(|a| format!("{} {}", c_param(a), a.name))
                .collect();
            let result = i.result.as_ref().map_or_else(|| "void".to_owned(), c_value);
            (
                i.c_name.clone(),
                format!("{result} {}({})", i.c_name, params.join(", ")),
            )
        })
        .collect()
}

/// The C type of a value (an input scalar or a result), IEEE 1800-2023 Table H.1.
fn c_value(ty: &DpiType) -> String {
    let int = |base: &str, signed: bool| {
        if signed {
            base.to_owned()
        } else {
            format!("unsigned {base}")
        }
    };
    match ty {
        DpiType::Bit => "svBit".to_owned(),
        DpiType::Logic => "svLogic".to_owned(),
        DpiType::BitVector { .. } => "svBitVecVal".to_owned(),
        DpiType::LogicVector { .. } => "svLogicVecVal".to_owned(),
        DpiType::Byte { signed } => int("char", *signed),
        DpiType::ShortInt { signed } => int("short", *signed),
        DpiType::Int { signed } => int("int", *signed),
        DpiType::LongInt { signed } => int("long long", *signed),
        DpiType::Chandle => "void*".to_owned(),
        DpiType::Unsupported(what) => format!("/* {what} */"),
    }
}

/// The C type of a parameter: vectors by pointer (`const` for inputs), outputs and `inout`
/// by pointer (§H.8).
fn c_param(arg: &DpiArg) -> String {
    let vector = matches!(
        arg.ty,
        DpiType::BitVector { .. } | DpiType::LogicVector { .. }
    );
    match (arg.dir, vector) {
        (Direction::Input, true) => format!("const {}*", c_value(&arg.ty)),
        (Direction::Input, false) => c_value(&arg.ty),
        (Direction::Output | Direction::Inout, _) => format!("{}*", c_value(&arg.ty)),
    }
}

/// A disagreement between the trampolines and Verilator's `V<prefix>__Dpi.h`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DpiHeaderMismatch {
    /// 1-based line in the header, or 0 when an import is missing from it.
    pub line: usize,
    /// What differs.
    pub message: String,
}

/// Checks every import's trampoline prototype against the `extern` declarations under
/// `// DPI IMPORTS` in `header` (the text of `V<prefix>__Dpi.h`). Argument names are not
/// compared; they do not reach the ABI. Imports with a type simjig does not implement are
/// skipped: no trampoline exists for them (the code generator rejects them, and a custom
/// emitter may leave them to C).
///
/// # Errors
/// The first import whose prototype differs from Verilator's, or that the header lacks.
pub fn check_dpi_header(imports: &[DpiImport], header: &str) -> Result<(), DpiHeaderMismatch> {
    let declared = header_imports(header);
    let implementable: Vec<DpiImport> = imports
        .iter()
        .filter(|i| check(i).is_ok())
        .cloned()
        .collect();
    for (c_name, expected) in dpi_prototypes(&implementable) {
        let Some((line, found)) = declared.iter().find(|(_, p)| p.name == c_name) else {
            return Err(DpiHeaderMismatch {
                line: 0,
                message: format!("DPI-C import `{c_name}` is not declared in the header"),
            });
        };
        let want = Prototype::parse(&expected).unwrap_or_default();
        // A task's C function returns `int`; the IR cannot tell it from a `void` function.
        let result_ok =
            found.result == want.result || (want.result == "void" && found.result == "int");
        if !result_ok || found.params != want.params {
            return Err(DpiHeaderMismatch {
                line: *line,
                message: format!(
                    "DPI-C import `{c_name}`: Verilator declares `{}`, but simjig's trampoline \
                     implements `{expected}`",
                    found.text
                ),
            });
        }
    }
    Ok(())
}

/// A C prototype split into the parts the ABI depends on.
#[derive(Debug, Default)]
struct Prototype {
    text: String,
    result: String,
    name: String,
    /// Parameter types without names, whitespace-normalised.
    params: Vec<String>,
}

impl Prototype {
    /// Parses `int f(const svBitVecVal* v, int x)`.
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_end_matches(';').trim();
        let (head, rest) = text.split_once('(')?;
        let params = rest.strip_suffix(')')?;
        let (result, name) = split_last_word(head)?;
        let params = params
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty() && *p != "void")
            .map(|p| split_last_word(p).map_or_else(|| normalise(p), |(ty, _)| ty))
            .collect();
        Some(Self {
            text: text.to_owned(),
            result,
            name: name.to_owned(),
            params,
        })
    }
}

/// `("const svBitVecVal*", "v")` from `const svBitVecVal* v`.
fn split_last_word(text: &str) -> Option<(String, &str)> {
    let text = text.trim();
    let at = text.rfind(|c: char| c.is_whitespace() || c == '*')?;
    let (ty, name) = text.split_at(at + 1);
    Some((normalise(ty), name.trim()))
}

/// Single spaces, and `*` attached to the type (`int *` is `int*`).
fn normalise(ty: &str) -> String {
    ty.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" *", "*")
}

/// `(line, prototype)` of each `extern` declaration after `// DPI IMPORTS`.
fn header_imports(header: &str) -> Vec<(usize, Prototype)> {
    let mut in_imports = false;
    let mut out = Vec::new();
    for (i, line) in header.lines().enumerate() {
        let line = line.trim();
        if line.starts_with("// DPI IMPORTS") {
            in_imports = true;
        } else if line.starts_with("// DPI EXPORTS") {
            in_imports = false;
        } else if in_imports
            && let Some(decl) = line.strip_prefix("extern ")
            && let Some(p) = Prototype::parse(decl)
        {
            out.push((i + 1, p));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototypes_parse_without_names() {
        let p =
            Prototype::parse("svBit f_bit(svBit b, svLogic l, svBit* ob, const svBitVecVal *v);")
                .unwrap();
        assert_eq!(p.result, "svBit");
        assert_eq!(p.name, "f_bit");
        assert_eq!(
            p.params,
            ["svBit", "svLogic", "svBit*", "const svBitVecVal*"]
        );
        let p = Prototype::parse("void* f_ch(void* c)").unwrap();
        assert_eq!(
            (p.result.as_str(), p.params.as_slice()),
            ("void*", ["void*".to_owned()].as_slice())
        );
        let p = Prototype::parse("int t()").unwrap();
        assert_eq!(p.params, Vec::<String>::new());
        let p = Prototype::parse("unsigned long long f(unsigned long long x)").unwrap();
        assert_eq!(
            (p.result.as_str(), p.params.as_slice()),
            (
                "unsigned long long",
                ["unsigned long long".to_owned()].as_slice()
            )
        );
    }
}
