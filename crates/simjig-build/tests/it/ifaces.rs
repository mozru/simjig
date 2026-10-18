//! An interface-ported DUT without a `.bind_iface` for every interface port fails with one
//! line per unbound port and the call that binds it, not Verilator's dump.

use simjig_build::Dut;

use crate::support::{Sandbox, dut};

fn iface_top() -> Dut {
    let d = dut("iface_top");
    Dut::new("iface_top")
        .sources([
            d.join("iface_pkg.sv"),
            d.join("bus_if.sv"),
            d.join("iface_top.sv"),
        ])
        .top("iface_top")
}

#[test]
fn no_bindings_lists_every_interface_port() {
    let s = Sandbox::new();
    let err = iface_top().build_in(&s.env()).unwrap_err();
    assert_eq!(
        format!("{err:?}"),
        "invalid DUT `iface_top` configuration: \
         port `in_bus` of `iface_top` is an interface port (`bus_if.dst`); add \
         `.bind_iface(\"in_bus\", \"bus_if.dst\")`\n\
         port `out_bus` of `iface_top` is an interface port (`bus_if.src`); add \
         `.bind_iface(\"out_bus\", \"bus_if.src\")`"
    );
}

#[test]
fn a_partial_binding_lists_the_unbound_ports() {
    let s = Sandbox::new();
    let err = iface_top()
        .bind_iface("in_bus", "bus_if.dst")
        .build_in(&s.env())
        .unwrap_err();
    assert_eq!(
        format!("{err:?}"),
        "invalid DUT `iface_top` configuration: \
         port `out_bus` of `iface_top` is an interface port (`bus_if.src`); add \
         `.bind_iface(\"out_bus\", \"bus_if.src\")`"
    );
}
