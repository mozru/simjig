# flop

D flip-flop with a synchronous, active-high reset: on `posedge clk`, `q <= rst ? 0 : d`.

- Top: `flop`. Sources: `flop.sv`.
- Conformance: `tests/conformance/tb/test_flop.py` (`edge_read`, `readonly_read`,
  `write_after_edge`, `timeout`).
