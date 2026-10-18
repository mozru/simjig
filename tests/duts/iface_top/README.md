# iface_top

A module whose data ports are SystemVerilog interface modports, so it cannot be a
Verilator top as-is.

- `iface_pkg`: `localparam int DATA_W = 16`; `typedef enum logic [1:0] { KIND_IDLE,
  KIND_DATA, KIND_LAST } kind_e`.
- `bus_if`: `valid`, `ready`, `data[DATA_W]`, `kind`; modports `src` (drives valid, data,
  kind; reads ready) and `dst` (the reverse).
- `iface_top(clk, rst, bus_if.dst in_bus, bus_if.src out_bus)`: a one-entry register
  slice; an accepted beat appears on `out_bus` next cycle with `data + 1`.
  `in_bus.ready = !full || out_bus.ready`.
- `iface_top_wrap`: a hand-written reference wrapper that instantiates two `bus_if`s and
  exposes flat ports (`in_valid`, `in_ready`, `in_data`, `in_kind`, `out_*`). It is the
  shape simjig-build's generated wrapper must take.

- Top: `iface_top` (lint only) or `iface_top_wrap` (simulation). Sources, in order:
  `iface_pkg.sv`, `bus_if.sv`, `iface_top.sv`, and `iface_top_wrap.sv` for the wrapper.
- Smoke: `tests/duts/smoke` (`iface_top_wrap`).
