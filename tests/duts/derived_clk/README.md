# derived_clk

`div_clk` is a flop toggled on every `posedge clk`, so it rises on every second clk edge,
in the same eval. `fast_count` counts clk edges; `slow_count` counts `div_clk` edges.
The slow domain takes `rst` asynchronously because `div_clk` is held low in reset
(`SYNCASYNCNET` waived in the source for that reason).

Triggers on `div_clk` are triggers on a derived clock (DECISION.md §1). In cocotb they see
post-eval values, unlike `RisingEdge(clk)`: see `tests/conformance/README.md`.

- Top: `derived_clk`. Sources: `derived_clk.sv`.
- Conformance: `tests/conformance/tb/test_derived_clk.py` (`derived_edge`).
- Smoke: `tests/duts/smoke` (`derived_clk`).
