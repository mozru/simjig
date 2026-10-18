//! Verilator's JSON-pass diagnostics about the interface ports of a DUT, turned into
//! configuration errors that name the `bind_iface` call to add or fix, instead of
//! Verilator's dump.

use crate::Error;
use crate::dut::DutConfig;

/// Verilator's diagnostics for an interface port left unconnected (5.052 `V3LinkDot.cpp`,
/// `V3Inst.cpp`; checked for both): with no binding at all the DUT is the top, and each
/// interface port is reported at its declaration; under the elaboration top, a port without
/// a binding is reported by name, and the `PINMISSING` warning before it gives its
/// declaration.
const IFACE_PORT_ON_TOP: &str = ": Unsupported: Interfaced port on top level module";
const IFACE_PORT_OPEN: (&str, &str) = (
    "Interface port '",
    "' is not connected to interface/modport pin expression",
);
const MISSING_PIN: &str = "Instance has missing pin: '";
const PORT_DECL: &str = ": ... Location of port declaration";

/// When the JSON pass failed because interface ports of the DUT have no
/// [`crate::Dut::bind_iface`], the error that lists each one with the call that binds it,
/// instead of Verilator's dump (which ends in an internal error).
///
/// Why from stderr: Verilator elaborates no design whose interface ports are unconnected,
/// so no JSON exists to read them from, and `--lint-only` (which accepts them) writes no
/// JSON. The declaration's source line gives the interface and modport.
pub(crate) fn unbound_iface_ports(config: &DutConfig, e: &svtypes::error::Error) -> Option<Error> {
    let svtypes::error::Error::Tool { stderr, .. } = e else {
        return None;
    };
    let top = config.top.as_deref().unwrap_or_default();
    let mut decls: Vec<(String, &str)> = Vec::new(); // (port, declaration loc) from PINMISSING
    let mut pending: Option<String> = None;
    // (port, `interface.modport`); the port is `Err(location)` when its declaration could
    // not be read, which still leaves the other ports' lines worth showing.
    let mut ports: Vec<(Result<String, &str>, Option<String>)> = Vec::new();
    for diag in stderr.lines() {
        if let Some(loc) = diag
            .strip_prefix("%Error-UNSUPPORTED: ")
            .and_then(|d| d.strip_suffix(IFACE_PORT_ON_TOP))
        {
            ports.push(match iface_port_at(loc) {
                Some((port, spec)) => (Ok(port), spec),
                None => (Err(loc), None),
            });
        } else if let Some((_, rest)) = diag.split_once(MISSING_PIN) {
            pending = rest.strip_suffix('\'').map(str::to_owned);
        } else if let Some(loc) = diag.trim_start().strip_suffix(PORT_DECL)
            && let Some(port) = pending.take()
        {
            decls.push((port, loc));
        } else if let Some((_, rest)) = diag.split_once(IFACE_PORT_OPEN.0)
            && let Some(port) = rest.strip_suffix(IFACE_PORT_OPEN.1)
        {
            let decl = decls.iter().find(|(p, _)| p == port);
            let spec = decl
                .and_then(|(_, loc)| iface_port_at(loc))
                .and_then(|(_, s)| s);
            ports.push((Ok(port.to_owned()), spec));
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for (port, spec) in ports {
        let port = match port {
            Ok(port) => port,
            Err(loc) => {
                lines.push(format!(
                    "an interface port of `{top}` (declared at {loc}) is not bound; add \
                     `.bind_iface(\"<port>\", \"<interface>.<modport>\")`"
                ));
                continue;
            }
        };
        if config.ifaces.iter().any(|b| b.port == port) {
            return None; // bound, yet reported: not the mistake this explains
        }
        let line = match spec {
            Some(spec) => format!(
                "port `{port}` of `{top}` is an interface port (`{spec}`); add \
                 `.bind_iface(\"{port}\", \"{spec}\")`"
            ),
            None => format!(
                "port `{port}` of `{top}` is an interface port; add \
                 `.bind_iface(\"{port}\", \"<interface>.<modport>\")`"
            ),
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    (!lines.is_empty()).then(|| Error::Config {
        dut: config.name.clone(),
        msg: lines.join("\n"),
    })
}

/// Verilator's diagnostics for a binding the generated elaboration top gets wrong
/// (5.052 `V3LinkCells.cpp`, `V3LinkDot.cpp`, `V3Width.cpp`; checked for each): a port the
/// DUT does not have (`PINNOTFOUND`), an interface the sources do not declare, and an
/// interface other than the port's.
const PIN_NOT_FOUND: &str = ": Pin not found: '";
const NO_INTERFACE: &str = ": Cannot find file containing interface: '";
const WRONG_INTERFACE: (&str, &str, &str) =
    (": Port '", "' expects '", "' interface but pin connects '");

/// When the JSON pass failed on a [`crate::Dut::bind_iface`] mistake, the error that says
/// which call to fix and how, instead of Verilator's dump of the generated top.
pub(crate) fn binding_mistakes(config: &DutConfig, e: &svtypes::error::Error) -> Option<Error> {
    let svtypes::error::Error::Tool { stderr, .. } = e else {
        return None;
    };
    let top = config.top.as_deref().unwrap_or_default();
    let call = |port: &str| {
        let b = config.ifaces.iter().find(|b| b.port == port)?;
        let spec = match &b.modport {
            Some(m) => format!("{}.{m}", b.interface),
            None => b.interface.clone(),
        };
        Some(format!("`.bind_iface(\"{port}\", \"{spec}\")`"))
    };
    // The DUT's ports the generated top left unconnected: what a mistyped port meant.
    let missing: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.split_once(MISSING_PIN)?.1.strip_suffix('\''))
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for diag in stderr.lines() {
        let line = if let Some(pin) = quoted_after(diag, PIN_NOT_FOUND) {
            call(pin).map(|_| {
                let hint = crate::codegen::closest(pin, missing.iter().copied())
                    .map(|c| format!("; did you mean `{c}`?"))
                    .unwrap_or_default();
                format!("no interface port `{pin}` on `{top}`{hint}")
            })
        } else if let Some(name) = quoted_after(diag, NO_INTERFACE) {
            let b = config.ifaces.iter().find(|b| b.interface == name);
            b.and_then(|b| call(&b.port)).map(|call| {
                format!(
                    "{call} names the interface `{name}`, which no source declares; add its \
                     source with `.sources([..])`"
                )
            })
        } else if let Some((port, want, got)) = wrong_interface(diag) {
            call(port).map(|call| {
                format!(
                    "port `{port}` of `{top}` is a `{want}` interface port, but {call} \
                     connects `{got}`; bind it as `{want}.<modport>`"
                )
            })
        } else {
            None
        };
        if let Some(line) = line.filter(|l| !lines.contains(l)) {
            lines.push(line);
        }
    }
    (!lines.is_empty()).then(|| Error::Config {
        dut: config.name.clone(),
        msg: lines.join("\n"),
    })
}

/// The `'quoted'` name right after `marker` at the end of `diag`.
fn quoted_after<'d>(diag: &'d str, marker: &str) -> Option<&'d str> {
    diag.split_once(marker)?.1.strip_suffix('\'')
}

/// (port, expected interface, connected interface) of a wrong-interface diagnostic.
fn wrong_interface(diag: &str) -> Option<(&str, &str, &str)> {
    let (pre, expects, connects) = WRONG_INTERFACE;
    let rest = diag.split_once(pre)?.1;
    let (port, rest) = rest.split_once(expects)?;
    let (want, rest) = rest.split_once(connects)?;
    Some((port, want, rest.strip_suffix("' interface")?))
}

/// The port named at `file:line:col` and, when the declaration has the usual
/// `interface[.modport] name` shape on that line, its `interface[.modport]`.
fn iface_port_at(loc: &str) -> Option<(String, Option<String>)> {
    let mut parts = loc.rsplitn(3, ':');
    let col: usize = parts.next()?.parse().ok()?;
    let line: usize = parts.next()?.parse().ok()?;
    let text = std::fs::read_to_string(parts.next()?).ok()?;
    let src = text.lines().nth(line.checked_sub(1)?)?;
    let start = src.char_indices().nth(col.checked_sub(1)?)?.0;
    let port: String = src[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if port.is_empty() {
        return None;
    }
    // `bus_if.src out_bus` or `bus_if.src a, out_bus`: the last word not ending in `,`.
    let spec = src[..start]
        .split_whitespace()
        .rev()
        .find(|w| !w.ends_with(','))
        .filter(|w| {
            *w != "interface" // a generic interface port names no interface
                && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        })
        .map(str::to_owned);
    Some((port, spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_error(stderr: String) -> svtypes::error::Error {
        svtypes::error::Error::Tool {
            command: "verilator".into(),
            status: Some(1),
            stderr,
        }
    }

    /// A declaration that cannot be read (a vanished or generated file) still leaves the
    /// lines for the other ports, and a line for itself.
    #[test]
    fn an_unreadable_declaration_keeps_the_other_ports() {
        let t = tempfile::tempdir().unwrap();
        let sv = t.path().join("top.sv");
        std::fs::write(&sv, "module top(\n  bus_if.src out_bus\n);\nendmodule\n").unwrap();
        let stderr = format!(
            "%Error-UNSUPPORTED: /gone/top.sv:3:14{IFACE_PORT_ON_TOP}\n\
             %Error-UNSUPPORTED: {}:2:14{IFACE_PORT_ON_TOP}\n",
            sv.display()
        );
        let config = DutConfig {
            name: "d".into(),
            top: Some("top".into()),
            ..DutConfig::default()
        };
        let err = unbound_iface_ports(&config, &tool_error(stderr)).unwrap();
        assert_eq!(
            err.to_string(),
            "invalid DUT `d` configuration: an interface port of `top` (declared at \
             /gone/top.sv:3:14) is not bound; add `.bind_iface(\"<port>\", \
             \"<interface>.<modport>\")`\n\
             port `out_bus` of `top` is an interface port (`bus_if.src`); add \
             `.bind_iface(\"out_bus\", \"bus_if.src\")`"
        );
    }
}
