# dpi

Imports two DPI-C functions:

- `pure function int dpi_add(int x, int y)`: drives `sum` combinationally.
- `context function int dpi_tick()`: called once per `posedge clk`; `calls` holds its
  return value (the number of calls so far, from 1).

`dpi_impl.c` is a reference implementation. Verilator's generated makefile compiles `.c`
files with the C++ compiler, so the file declares C linkage under `__cplusplus`; without it
the link fails with undefined `_dpi_add`/`_dpi_tick`.

- Top: `dpi`. Sources: `dpi.sv`, plus `dpi_impl.c` on the Verilator command line.
- Smoke: `tests/duts/smoke` (`dpi`).
