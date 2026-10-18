//! An interface-ported DUT without a `.bind_iface` for every interface port, or with a
//! wrong one, fails with one line per mistake and the call that fixes it, not Verilator's
//! dump.

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

/// A binding mistake Verilator catches while elaborating the generated top reads as the
/// `bind_iface` call to fix, not as a dump of the generated file.
#[test]
fn binding_mistakes_name_the_bind_iface_to_fix() {
    for (port, iface, want) in [
        (
            "in_bs",
            "bus_if.dst",
            "invalid DUT `iface_top` configuration: no interface port `in_bs` on \
             `iface_top`; did you mean `in_bus`?",
        ),
        (
            "in_bus",
            "other_if.dst",
            "invalid DUT `iface_top` configuration: port `in_bus` of `iface_top` is a \
             `bus_if` interface port, but `.bind_iface(\"in_bus\", \"other_if.dst\")` \
             connects `other_if`; bind it as `bus_if.<modport>`",
        ),
        (
            "in_bus",
            "nope_if.dst",
            "invalid DUT `iface_top` configuration: `.bind_iface(\"in_bus\", \
             \"nope_if.dst\")` names the interface `nope_if`, which no source declares; add \
             its source with `.sources([..])`",
        ),
    ] {
        // One sandbox each: a build script may build one DUT name in one configuration.
        let s = Sandbox::new();
        let other = s.path().join("other_if.sv");
        std::fs::write(
            &other,
            "interface other_if;\n  logic x;\n  modport dst(input x);\nendinterface\n",
        )
        .unwrap();
        let err = iface_top()
            .sources([other])
            .bind_iface(port, iface)
            .bind_iface("out_bus", "bus_if.src")
            .build_in(&s.env())
            .unwrap_err();
        assert_eq!(format!("{err:?}"), want, "{port} {iface}");
    }
}
