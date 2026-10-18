//! The flat-port wrapper of an interface-ported DUT (DECISION.md table 2, row 9).
//!
//! Verilator rejects interface ports on the top module, so the model top is a generated
//! wrapper that instantiates each bound interface and the DUT, and exposes every modport
//! signal as a plain port named `<port>_<signal>`. Interface ports of the interface itself
//! (`interface bus_if(input logic clk)`) are connected at the instance; every other modport
//! signal is joined to its flat port with a continuous assignment, in the direction the
//! modport gives it from the DUT's side.
//!
//! The plan is computed from the DUT module's entry in the IR, which exists both in the
//! design elaborated under [`elab_module`] (before the wrapper exists) and in the design
//! elaborated under the wrapper, so both give the same wrapper.

use std::collections::BTreeSet;

use svtypes::ir::{Design, Direction, Module};

use super::super::{Error, IfaceBinding, IfaceTop, Wrapper, WrapperIface, WrapperPort};
use super::sv::{Mode, spell};

/// The wrapper's module name.
#[must_use]
pub fn wrapper_module(dut: &str) -> String {
    format!("simjig_wrap_{dut}")
}

/// The module name of the elaboration-only top (written by `codegen::emit_elab_top`).
#[must_use]
pub fn elab_module(dut: &str) -> String {
    format!("simjig_elab_{dut}")
}

/// The DUT module of `config` in `design`.
pub(crate) fn dut_module<'d>(design: &'d Design, config: &IfaceTop) -> Result<&'d Module, Error> {
    let found: Vec<&Module> = design
        .modules
        .iter()
        .filter(|m| m.orig_name == config.dut)
        .collect();
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::config(
            format!("iface top `{}`", config.dut),
            "no such module in the design; elaborate it under the elaboration top first",
        )),
        _ => Err(Error::config(
            format!("iface top `{}`", config.dut),
            "the module is specialised more than once in this design",
        )),
    }
}

/// Plans the wrapper for `config` from the DUT module in `design`.
///
/// # Errors
/// [`Error::Config`] when bindings and interface ports do not match one to one, or a
/// generic interface port has no modport; [`Error::Unsupported`] for a port type the wrapper
/// cannot spell or an `inout` modport signal.
pub fn lower_wrapper(design: &Design, config: &IfaceTop) -> Result<Wrapper, Error> {
    let dut = dut_module(design, config)?;
    let mut wrapper = Wrapper {
        module: wrapper_module(&config.dut),
        dut: config.dut.clone(),
        dut_params: config.params.clone(),
        ports: Vec::new(),
        dut_conns: Vec::new(),
        ifaces: Vec::new(),
    };
    for port in &dut.ports {
        let (packed, unpacked) = spell(design, port.ty, Mode::Wrapper)?;
        wrapper.ports.push(WrapperPort {
            name: port.name.clone(),
            dir: port.dir,
            packed,
            unpacked,
        });
        wrapper
            .dut_conns
            .push((port.name.clone(), port.name.clone()));
    }
    for binding in &config.bindings {
        if !dut.iface_ports.iter().any(|p| p.name == binding.port) {
            return Err(Error::config(
                format!("bind_iface({})", binding.port),
                format!("`{}` has no interface port of that name", config.dut),
            ));
        }
    }
    for iface_port in &dut.iface_ports {
        let binding = config
            .bindings
            .iter()
            .find(|b| b.port == iface_port.name)
            .ok_or_else(|| {
                let iface = &design.module(iface_port.interface).orig_name;
                Error::config(
                    format!("interface port `{}.{}`", config.dut, iface_port.name),
                    format!(
                        "not bound; add `.bind_iface(\"{}\", \"{iface}\")`",
                        iface_port.name
                    ),
                )
            })?;
        let iface = design.module(iface_port.interface);
        let modport = choose_modport(&config.dut, iface, iface_port.modport.as_deref(), binding)?;
        let plan = plan_iface(design, iface, modport, binding, &mut wrapper.ports)?;
        wrapper.ifaces.push(plan);
    }
    check_unique(&wrapper)?;
    Ok(wrapper)
}

/// The modport the bundle exposes: the declared one, or the binding's for a generic port.
fn choose_modport<'m>(
    dut: &str,
    iface: &'m Module,
    declared: Option<&str>,
    binding: &IfaceBinding,
) -> Result<&'m svtypes::ir::Modport, Error> {
    let item = || format!("bind_iface({})", binding.port);
    if binding.interface != iface.orig_name {
        return Err(Error::config(
            item(),
            format!(
                "`{dut}.{}` is a `{}` port, not `{}`",
                binding.port, iface.orig_name, binding.interface
            ),
        ));
    }
    let name = match (declared, binding.modport.as_deref()) {
        (Some(d), Some(b)) if d != b => {
            return Err(Error::config(
                item(),
                format!("the port declares modport `{d}`, not `{b}`"),
            ));
        }
        (Some(name), _) | (None, Some(name)) => name,
        (None, None) => {
            let names: Vec<&str> = iface.modports.iter().map(|m| m.name.as_str()).collect();
            return Err(Error::config(
                item(),
                format!(
                    "a generic interface port needs a modport; one of: {}",
                    names.join(", ")
                ),
            ));
        }
    };
    iface
        .modports
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| {
            Error::config(
                item(),
                format!("`{}` has no modport `{name}`", iface.orig_name),
            )
        })
}

/// Plans one interface instance and appends its flat ports.
fn plan_iface(
    design: &Design,
    iface: &Module,
    modport: &svtypes::ir::Modport,
    binding: &IfaceBinding,
    ports: &mut Vec<WrapperPort>,
) -> Result<WrapperIface, Error> {
    let inst = &binding.port;
    let flat = |signal: &str| format!("{inst}_{signal}");
    let mut plan = WrapperIface {
        instance: inst.clone(),
        interface: iface.orig_name.clone(),
        modport: modport.name.clone(),
        params: binding.params.clone(),
        port_conns: Vec::new(),
        assigns: Vec::new(),
        members: Vec::new(),
    };
    let is_iface_port = |name: &str| iface.ports.iter().any(|p| p.name == name);
    let mut signals: Vec<(&str, Direction, svtypes::ir::TypeId)> = modport
        .members
        .iter()
        .map(|m| (m.name.as_str(), m.dir, m.ty))
        .collect();
    for p in &iface.ports {
        if !signals.iter().any(|(n, _, _)| *n == p.name) {
            signals.push((&p.name, p.dir, p.ty));
        }
    }
    for (name, dir, ty) in signals {
        let port = flat(name);
        let (packed, unpacked) = spell(design, ty, Mode::Wrapper)?;
        if is_iface_port(name) {
            plan.port_conns.push((name.to_owned(), port.clone()));
        } else {
            match dir {
                Direction::Input => plan.assigns.push((format!("{inst}.{name}"), port.clone())),
                Direction::Output => plan.assigns.push((port.clone(), format!("{inst}.{name}"))),
                Direction::Inout => {
                    return Err(Error::unsupported(
                        format!(
                            "modport signal `{}.{}.{name}`",
                            iface.orig_name, modport.name
                        ),
                        "inout modport signals cannot be joined with an assignment",
                    ));
                }
            }
        }
        plan.members.push((name.to_owned(), port.clone(), dir));
        ports.push(WrapperPort {
            name: port,
            dir,
            packed,
            unpacked,
        });
    }
    Ok(plan)
}

/// Flat names `<port>_<signal>` can collide with data ports or with each other.
fn check_unique(wrapper: &Wrapper) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for port in &wrapper.ports {
        if !seen.insert(port.name.as_str()) {
            return Err(Error::config(
                format!("wrapper `{}`", wrapper.module),
                format!(
                    "two wrapper ports are both named `{}`; rename a port or signal",
                    port.name
                ),
            ));
        }
    }
    Ok(())
}
