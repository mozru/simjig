# timing_dut

Needs `--timing`. The design schedules its own events, so the next event time comes from
`nextTimeSlot()`, not only from the testbench.

- `iclk` (seen as `iclk_out`): internal clock from `initial forever #5 iclk = ~iclk`;
  10 ns period, rising edges at 5, 15, 25, ... ns.
- `ticks`: counts `iclk` rising edges (5 at 50 ns).
- `done`: goes low on each rising edge of `start`, high again 23 ns later.

Timescale is fixed in the source (`timeunit 1ns; timeprecision 1ps`). Without `--timing`,
Verilator warns `STMTDLY` and ignores the delays.

- Top: `timing_dut`. Sources: `timing_dut.sv`. Flags: `--timing`.
- Smoke: `tests/duts/smoke` (`timing_dut`).
