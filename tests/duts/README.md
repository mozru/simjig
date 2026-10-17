# Shared DUTs

SystemVerilog designs that every simjig stage tests against. Each directory holds the RTL
and a README giving the top module, source order, parameters and behaviour.

| DUT | What it exercises |
|---|---|
| `inv` | combinational, no clock |
| `flop` | D flip-flop |
| `counter` | counter, parameter `WIDTH` |
| `comb_chain` | input-to-output path through several assigns |
| `fifo` | valid/ready FIFO, parameter `DEPTH` |
| `struct_pipe` | package with packed struct, enum, localparam |
| `wide` | 1-, 8-, 33-, 64-, 65-, 200-bit ports |
| `async_fifo` | two independent clocks, CDC |
| `timing_dut` | `#` delays and an internal clock (`--timing`) |
| `fatal` | `$fatal` / `$stop` / `$error` / `$finish` |
| `iface_top` | interface ports, modports, package; reference flat wrapper |
| `dpi` | DPI-C imports |
| `derived_clk` | internal divided clock |

- `pixi run duts-lint`: lints every design with `verilator --lint-only -Wall` (at several
  parameter values where a design has them). `lint.sh` is also the machine-readable list
  of tops, source order and flags.
- `pixi run -e conformance duts-smoke`: builds every DUT not covered by the conformance
  goldens with cocotb and checks the behaviour its README promises.
- The conformance goldens live in `tests/conformance`.
