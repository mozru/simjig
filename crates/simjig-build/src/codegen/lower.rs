//! The lowering: the IR plus the [`CodegenConfig`] in, a [`Lowered`] out.
//!
//! This is the only place that makes naming, slot, storage and access decisions:
//! - slots: the top's ports in declaration order, then the probes in configuration order;
//! - storage: the IR's [`Storage`] of each port, or `packed_storage(width)` for a probe;
//! - access: from the port direction (an `input` is driven by the testbench), or from the
//!   probe's [`ProbeAccess`];
//! - for an interface-ported DUT, the wrapper's flat ports are grouped back into one
//!   bundle field per interface port. Bundle members keep the modport's direction, which
//!   is the DUT's view: the testbench drives what the DUT reads.
//!
//! Type names are decided in [`types`], identifier rules in [`names`](super::names).

mod path;
mod sv;
mod types;
mod wrapper;

use svtypes::ir::{Design, Param, ParamValue, Storage, TypeId, TypeKind};
use svtypes::width::packed_storage;

use self::path::check_probe_path;
use self::sv::{LayoutSpeller, nameable};
use self::types::{Root, Types, Use, bits, lower_types};
use self::wrapper::dut_module;
pub use self::wrapper::{DUT_INSTANCE, elab_module, lower_wrapper, wrapper_module};
use super::names::{Namespace, camel, screaming, snake};
use super::{
    Access, Bundle, BundleMember, CodegenConfig, Const, ConstGroup, ConstValue, DutField, Error,
    ItemKind, Layout, LayoutCheck, LayoutMember, LayoutValue, Lowered, Probe, ProbeAccess,
    ProbeType, Signal, SignalTarget, Wrapper, WrapperIface,
};

/// Lowers `design` for `config`.
///
/// `design.top` must be the model top: the DUT itself, or, when
/// [`CodegenConfig::iface_top`] is set, the generated wrapper elaborated as top.
///
/// # Errors
/// [`Error::Unsupported`] for IR constructs without a generated form (unpacked struct
/// ports, enums above 128 bits, ...); [`Error::Config`] when probes or interface bindings
/// do not match the design.
pub fn lower(design: &Design, config: &CodegenConfig) -> Result<Lowered, Error> {
    let top = design.top_module();
    let wrapper = config
        .iface_top
        .as_ref()
        .map(|t| lower_wrapper(design, t))
        .transpose()?;
    if let Some(w) = &wrapper
        && top.orig_name != w.module
    {
        return Err(Error::config(
            format!("iface top `{}`", w.dut),
            format!(
                "the design's top is `{}`; lower the design elaborated with `{}` as top",
                top.orig_name, w.module
            ),
        ));
    }
    let types = lower_types(design, &roots(design, config)?)?;
    let mut types = types;
    let (signals, fields, bundles) = lower_signals(design, config, &mut types, wrapper.as_ref())?;
    let packages = lower_packages(design, &types)?;
    let mut param_ns = Namespace::with_reserved(&["SIGNALS", "IR_HASH"]);
    // The wrapper has no parameters: an interface-ported DUT's come from its own module.
    let params_of = match &config.iface_top {
        Some(t) => dut_module(design, t)?,
        None => top,
    };
    let dut_params = params_of
        .params
        .iter()
        .map(|p| lower_const(design, &types, p, &mut param_ns))
        .collect::<Result<_, _>>()?;
    let layout = lower_layout(design, &types);
    Ok(Lowered {
        top: top.orig_name.clone(),
        producer: format!("{} {}", design.producer.tool, design.producer.version),
        ir_hash: crate::hash::ir_hash(design, config),
        items: types.items.into_iter().map(|(_, item)| item).collect(),
        packages,
        dut_params,
        signals,
        fields,
        bundles,
        layout,
        wrapper,
    })
}

/// The traversal roots: ports and probes (required), then package typedefs (optional).
fn roots(design: &Design, config: &CodegenConfig) -> Result<Vec<Root>, Error> {
    let top = design.top_module();
    let mut roots: Vec<Root> = top
        .ports
        .iter()
        .map(|p| Root {
            ty: p.ty,
            site: Use {
                path: format!("{}.{}", top.orig_name, p.name),
                candidates: vec![
                    camel(&p.name),
                    camel(&format!("{}_{}", top.orig_name, p.name)),
                ],
            },
            required: true,
        })
        .collect();
    for probe in &config.probes {
        if let Some(ty) = probe_type(design, probe)? {
            let leaf = probe.path.rsplit('.').next().unwrap_or(&probe.path);
            roots.push(Root {
                ty,
                site: Use {
                    path: probe.path.clone(),
                    candidates: vec![camel(leaf), camel(&probe.path)],
                },
                required: true,
            });
        }
    }
    for package in &design.packages {
        roots.extend(package.typedefs.iter().map(|&ty| Root {
            ty,
            site: Use {
                path: package.name.clone(),
                candidates: Vec::new(),
            },
            required: false,
        }));
    }
    Ok(roots)
}

/// The IR type a probe names, or `None` for a plain vector.
fn probe_type(design: &Design, probe: &Probe) -> Result<Option<TypeId>, Error> {
    match &probe.ty {
        ProbeType::Bits { width } if *width >= 1 => Ok(None),
        ProbeType::Bits { .. } => Err(Error::probe(&probe.path, "width must be at least 1")),
        ProbeType::Named(name) => design
            .types
            .iter()
            .position(|t| t.name.as_deref() == Some(name))
            .map(|i| Some(TypeId(u32::try_from(i).unwrap_or(u32::MAX))))
            .ok_or_else(|| Error::probe(&probe.path, format!("no type named `{name}` in the IR"))),
    }
}

/// The packed width of `id`, looking through unpacked dimensions.
fn element_width(design: &Design, mut id: TypeId) -> u32 {
    while let TypeKind::UnpackedArray { elem, .. } = design.ty(id).kind {
        id = elem;
    }
    design.ty(id).width.unwrap_or(0)
}

type SignalParts = (Vec<Signal>, Vec<DutField>, Vec<Bundle>);

/// Slots, `Dut` fields and bundles.
fn lower_signals(
    design: &Design,
    config: &CodegenConfig,
    types: &mut Types,
    wrapper: Option<&Wrapper>,
) -> Result<SignalParts, Error> {
    let top = design.top_module();
    let mut fields_ns = Namespace::default();
    let (mut signals, mut fields, mut bundles) = (Vec::new(), Vec::new(), Vec::<Bundle>::new());
    let mut index = 0;
    while let Some(port) = top.ports.get(index) {
        let iface = wrapper.and_then(|w| {
            w.ifaces
                .iter()
                .find(|i| i.members.first().is_some_and(|m| m.1 == port.name))
        });
        let group = iface.map_or(1, |i| i.members.len());
        let first_slot = slot(signals.len());
        for (k, port) in top.ports.iter().skip(index).take(group).enumerate() {
            if let Some(i) = iface
                && i.members.get(k).is_none_or(|m| m.1 != port.name)
            {
                return Err(Error::config(
                    format!("wrapper `{}`", top.orig_name),
                    format!("expected the ports of interface `{}` in order", i.instance),
                ));
            }
            signals.push(port_signal(design, types, port, slot(signals.len()))?);
        }
        if index + group > top.ports.len() {
            return Err(Error::config(
                format!("wrapper `{}`", top.orig_name),
                "interface ports missing",
            ));
        }
        match iface {
            None => fields.push(DutField::Signal {
                name: fields_ns.claim(&snake(&port.name)),
                slot: first_slot,
            }),
            Some(i) => {
                let bundle = bundle_for(types, &mut bundles, i, &signals[first_slot as usize..]);
                fields.push(DutField::Bundle {
                    name: fields_ns.claim(&snake(&i.instance)),
                    bundle,
                    first_slot,
                });
            }
        }
        index += group;
    }
    for probe in &config.probes {
        let (signal, name) = lower_probe(design, types, probe, slot(signals.len()), wrapper)?;
        fields.push(DutField::Signal {
            name: fields_ns.claim(&snake(&name)),
            slot: signal.slot,
        });
        signals.push(signal);
    }
    Ok((signals, fields, bundles))
}

/// The signal of top port `port`.
fn port_signal(
    design: &Design,
    types: &Types,
    port: &svtypes::ir::Port,
    slot: u32,
) -> Result<Signal, Error> {
    let top = &design.top_module().orig_name;
    let cpp_member = port.cpp_member.clone().ok_or_else(|| {
        Error::unsupported(
            format!("port {top}.{}", port.name),
            "top port without a C++ member",
        )
    })?;
    Ok(Signal {
        slot,
        name: format!("{top}.{}", port.name),
        target: SignalTarget::Port { cpp_member },
        ty: types.rust(design, port.ty),
        width: element_width(design, port.ty),
        storage: port.storage.clone(),
        access: Access::from_direction(port.dir),
    })
}

/// The bundle for interface instance `iface` whose signals start `signals`: an existing
/// one with the same modport and members (two ports of one interface and modport share a
/// type), or a new one.
fn bundle_for(
    types: &mut Types,
    bundles: &mut Vec<Bundle>,
    iface: &WrapperIface,
    signals: &[Signal],
) -> usize {
    let mut ns = Namespace::default();
    let members: Vec<BundleMember> = iface
        .members
        .iter()
        .zip(signals)
        .map(|((name, _, _), s)| BundleMember {
            name: ns.claim(&snake(name)),
            sv_name: name.clone(),
            ty: s.ty.clone(),
            access: s.access,
        })
        .collect();
    let sv_name = format!("{}.{}", iface.interface, iface.modport);
    if let Some(b) = bundles
        .iter()
        .position(|b| b.sv_name == sv_name && b.members == members)
    {
        return b;
    }
    let name = types
        .namespace
        .claim(&camel(&format!("{}_{}", iface.interface, iface.modport)));
    bundles.push(Bundle {
        name,
        sv_name,
        members,
    });
    bundles.len() - 1
}

/// A slot id; a model never has 2^32 signals.
fn slot(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// One probe's signal, and the base of its field name (the path below the top).
///
/// The path starts at the DUT the testbench sees. For an interface-ported DUT that is the
/// DUT module, not the wrapper Verilator elaborates as top: the wrapper is an
/// implementation detail, so its name is rejected, and the path is rewritten under the
/// wrapper's [`DUT_INSTANCE`] to name the scope Verilator registers.
fn lower_probe(
    design: &Design,
    types: &Types,
    probe: &Probe,
    slot: u32,
    wrapper: Option<&Wrapper>,
) -> Result<(Signal, String), Error> {
    let item = || format!("probe {}", probe.path);
    let segments: Vec<&str> = probe.path.split('.').collect();
    check_probe_path(design, probe, &segments, wrapper)
        .map_err(|why| Error::probe(&probe.path, why))?;
    let (ty, width) = match (probe_type(design, probe)?, &probe.ty) {
        (Some(id), _) => (types.rust(design, id), design.ty(id).width.unwrap_or(0)),
        (None, ProbeType::Bits { width }) => (bits(*width), *width),
        (None, ProbeType::Named(_)) => {
            return Err(Error::probe(&probe.path, "internal: unresolved type"));
        }
    };
    if width == 0 {
        return Err(Error::unsupported(item(), "probes of unpacked types"));
    }
    let storage: Storage = packed_storage(width).map_err(|e| Error::unsupported(item(), e.0))?;
    let var = segments[segments.len() - 1].to_owned();
    // Verilator names scopes by the plain instance names (`u__0` stays `u__0`), unlike
    // C++ members, so the path needs no encoding.
    let scope_path: Vec<&str> = match wrapper {
        Some(w) => [w.module.as_str(), DUT_INSTANCE]
            .into_iter()
            .chain(segments[1..segments.len() - 1].iter().copied())
            .collect(),
        None => segments[..segments.len() - 1].to_vec(),
    };
    let signal = Signal {
        slot,
        name: probe.path.clone(),
        target: SignalTarget::Probe {
            module: probe.module.clone(),
            var,
            scope: scope_path.join("."),
        },
        ty,
        width,
        storage,
        access: match probe.access {
            ProbeAccess::Read => Access::PublicRd,
            ProbeAccess::ReadWrite => Access::PublicRw,
        },
    };
    Ok((signal, segments[1..].join("_")))
}

/// Package parameters, one group per package that has any.
fn lower_packages(design: &Design, types: &Types) -> Result<Vec<ConstGroup>, Error> {
    let mut modules = Namespace::default();
    let mut out = Vec::new();
    for package in design.packages.iter().filter(|p| !p.params.is_empty()) {
        let mut ns = Namespace::default();
        let consts = package
            .params
            .iter()
            .map(|p| lower_const(design, types, p, &mut ns))
            .collect::<Result<_, _>>()?;
        out.push(ConstGroup {
            module: modules.claim(&snake(package.name.trim_start_matches('$'))),
            package: package.name.clone(),
            consts,
        });
    }
    Ok(out)
}

/// One parameter as a const.
fn lower_const(
    design: &Design,
    types: &Types,
    param: &Param,
    ns: &mut Namespace,
) -> Result<Const, Error> {
    let value = match &param.value {
        ParamValue::String(s) => ConstValue::Str(s.clone()),
        ParamValue::Real(text) => ConstValue::Real(real_literal(text).ok_or_else(|| {
            Error::unsupported(
                format!("parameter {}", param.name),
                format!("real value `{text}`"),
            )
        })?),
        ParamValue::Bits(lit) => match param.ty.map(|t| (t, types.rust(design, t))) {
            Some((t, super::RustType::Named(name)))
                if matches!(design.ty(t).kind, TypeKind::Enum { .. }) =>
            {
                ConstValue::Enum {
                    ty: name,
                    base: bits(lit.width()),
                    value: low_u128(lit.words()),
                }
            }
            _ => int_const(lit),
        },
        _ => {
            return Err(Error::unsupported(
                format!("parameter {}", param.name),
                super::error::UNKNOWN_IR_KIND,
            ));
        }
    };
    Ok(Const {
        name: ns.claim(&screaming(&param.name)),
        sv_name: param.name.clone(),
        value,
    })
}

/// The low 128 bits of little-endian 64-bit words.
fn low_u128(words: &[u64]) -> u128 {
    u128::from(words.first().copied().unwrap_or(0))
        | (u128::from(words.get(1).copied().unwrap_or(0)) << 64)
}

/// An integral parameter as the smallest Rust primitive that holds it (`bool` for one
/// unsigned bit), or `WideBits` above 128 bits.
fn int_const(lit: &svtypes::literal::Literal) -> ConstValue {
    let (width, signed) = (lit.width(), lit.is_signed());
    if width > 128 {
        let words = lit
            .words()
            .iter()
            .flat_map(|w| [w & 0xffff_ffff, w >> 32])
            .map(|w| u32::try_from(w).unwrap_or(0))
            .take(width.div_ceil(32) as usize)
            .collect();
        return ConstValue::Wide { width, words };
    }
    let raw = low_u128(lit.words());
    if width == 1 && !signed {
        return ConstValue::Int {
            ty: "bool",
            value: (raw == 1).to_string(),
        };
    }
    let bits = [8, 16, 32, 64, 128]
        .into_iter()
        .find(|&b| width <= b)
        .unwrap_or(128);
    let ty = match (signed, bits) {
        (false, 8) => "u8",
        (false, 16) => "u16",
        (false, 32) => "u32",
        (false, 64) => "u64",
        (false, _) => "u128",
        (true, 8) => "i8",
        (true, 16) => "i16",
        (true, 32) => "i32",
        (true, 64) => "i64",
        (true, _) => "i128",
    };
    let value = if signed && width < 128 && raw >> (width - 1) & 1 == 1 {
        // Two's complement: subtract 2^width. `raw < 2^width`, so this is in i128 range.
        (i128::try_from(raw).unwrap_or(0) - (1i128 << width)).to_string()
    } else if signed && width == 128 {
        i128::from_le_bytes(raw.to_le_bytes()).to_string()
    } else {
        raw.to_string()
    };
    ConstValue::Int { ty, value }
}

/// A Rust `f64` literal for the decimal text Verilator printed.
fn real_literal(text: &str) -> Option<String> {
    let t = text.trim();
    let numeric = t
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if !numeric || !t.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(if t.contains(['.', 'e', 'E']) {
        t.to_owned()
    } else {
        format!("{t}.0")
    })
}

/// The layout check: `$bits` of every generated type, member offsets of structs and
/// unions, and the constants of nameable enums.
fn lower_layout(design: &Design, types: &Types) -> Layout {
    let mut checks = Vec::new();
    let mut speller = LayoutSpeller::default();
    for (id, item) in &types.items {
        if item.storage.is_none() {
            continue;
        }
        let ty = design.ty(*id);
        // An enum that cannot be named is spelled as its base, so checking it is vacuous.
        let named = ty.name.as_deref().is_some_and(|n| nameable(design, n));
        if matches!(ty.kind, TypeKind::Enum { .. }) && !named {
            continue;
        }
        // Only an unpacked aggregate fails to spell, and those have no storage.
        let Ok(sv_type) = speller.spell(design, *id) else {
            continue;
        };
        let members = match &ty.kind {
            TypeKind::Struct { fields, .. } | TypeKind::Union { fields, .. }
                if !matches!(item.kind, ItemKind::Alias { .. }) =>
            {
                fields
                    .iter()
                    .map(|f| LayoutMember {
                        sv_name: f.name.clone(),
                        lsb: f.lsb.unwrap_or(0),
                        width: design.ty(f.ty).width.unwrap_or(0),
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let values = match (&ty.kind, ty.name.as_deref()) {
            (TypeKind::Enum { variants, .. }, Some(name))
                if nameable(design, name) && ty.alias_of.is_none() =>
            {
                let scope = name.rsplit_once("::").map_or("", |(s, _)| s);
                variants
                    .iter()
                    .map(|v| LayoutValue {
                        sv_expr: format!("{scope}::{}", v.name),
                        literal: v.value.to_string(),
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        checks.push(LayoutCheck {
            sv_name: item.sv_name.clone(),
            sv_type,
            width: item.width,
            members,
            values,
        });
    }
    Layout {
        module: format!("simjig_layout_{}", design.top_module().orig_name),
        typedefs: speller.into_typedefs(),
        checks,
    }
}

#[cfg(test)]
mod tests {
    use svtypes::literal::Literal;

    use super::*;

    #[test]
    fn int_consts_pick_the_smallest_primitive() {
        for (text, ty, value) in [
            ("32'shc", "i32", "12"),
            ("32'shfffffffb", "i32", "-5"),
            ("8'ha5", "u8", "165"),
            ("1'h1", "bool", "true"),
            ("12'h800", "u16", "2048"),
            ("5'sh10", "i8", "-16"),
            ("128'shffffffffffffffffffffffffffffffff", "i128", "-1"),
        ] {
            let lit: Literal = text.parse().unwrap();
            assert_eq!(
                int_const(&lit),
                ConstValue::Int {
                    ty,
                    value: value.into()
                },
                "{text}"
            );
        }
        let big: Literal = "130'h200000000000000000000000000000001".parse().unwrap();
        assert_eq!(
            int_const(&big),
            ConstValue::Wide {
                width: 130,
                words: vec![1, 0, 0, 0, 2]
            }
        );
    }

    #[test]
    fn real_literals() {
        assert_eq!(real_literal("0.25").as_deref(), Some("0.25"));
        assert_eq!(real_literal("3").as_deref(), Some("3.0"));
        assert_eq!(real_literal("1e-3").as_deref(), Some("1e-3"));
        assert_eq!(real_literal("inf"), None);
    }
}
