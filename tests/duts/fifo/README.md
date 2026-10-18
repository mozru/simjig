# fifo

Synchronous valid/ready FIFO with first-word fall-through. A beat enters when
`in_valid && in_ready` and leaves when `out_valid && out_ready`, both at `posedge clk`.
`level` is the occupancy; `in_ready = level != DEPTH`, `out_valid = level != 0`.
There is no combinational path from `out_ready` to `in_ready`.

- Top: `fifo`. Sources: `fifo.sv`.
- Parameters: `WIDTH` (default 8), `DEPTH` (default 4; a power of two, at least 2).
  Linted at DEPTH 2, 4 and 16.
- Conformance: `tests/conformance/tb/test_fifo.py` (`fifo_stream`, DEPTH=4).
