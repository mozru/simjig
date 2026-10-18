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
    let mut ports: Vec<(String, Option<String>)> = Vec::new();
    for diag in stderr.lines() {
        if let Some(loc) = diag
            .strip_prefix("%Error-UNSUPPORTED: ")
            .and_then(|d| d.strip_suffix(IFACE_PORT_ON_TOP))
        {
            ports.push(iface_port_at(loc)?);
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
            ports.push((port.to_owned(), spec));
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for (port, spec) in ports {
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
