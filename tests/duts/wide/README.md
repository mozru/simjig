# wide

One input and one output at each width Verilator stores differently: 1 and 8 bits
(`CData`), 33 and 64 bits (`QData`), 65 and 200 bits (`VlWide`). Each `oN = ~iN`
combinationally; `q200` is `i200` registered on `posedge clk`.

- Top: `wide`. Sources: `wide.sv`.
- Smoke: `tests/duts/smoke` (`wide`), which also checks each handle's width.
