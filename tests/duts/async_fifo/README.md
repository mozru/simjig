# async_fifo

Dual-clock FIFO (Cummings style): Gray-coded pointers, a two-flop synchronizer in each
direction, registered `wfull` and `rempty`, first-word fall-through `rdata`.

- Top: `async_fifo`. Sources: `async_fifo.sv`.
- Parameters: `WIDTH` (default 8), `DEPTH` (default 8; a power of two, **at least 4**).
- Clocks: `wclk` (write side) and `rclk` (read side), unrelated. `wrst`/`rrst` are
  synchronous to their own clock; assert both for at least two cycles of the slower clock.
- Latency: a word written at a `wclk` edge shows `rempty = 0` three `rclk` edges later
  (two synchronizer stages plus the registered flag).
- Internal nets read by the conformance tests: `wbin`, `rbin`, `rgray_w2`, `wgray_r2`.
- Conformance: `tests/conformance/tb/test_async_fifo.py` (`two_clocks`, `cdc_stream`).
