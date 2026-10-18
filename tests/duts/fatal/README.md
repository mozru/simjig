# fatal

Runs one of `$fatal`, `$stop`, `$error` or `$finish`, selected by `kind`
(0 = `$fatal`, 1 = `$stop`, 2 = `$error`, 3 = `$finish`):

- at run time: on a `posedge clk` with `fire = 1` (and `rst = 0`);
- at time 0: set parameter `AT_START` to 0..3 (default -1 = none); the task runs in an
  `initial` block, during the first eval.

`alive` counts clk edges (cleared by `rst`) so a testbench can tell whether the model kept
evaluating.

Observed under cocotb 2.1.0 on Verilator 5.052 (checked by `tests/duts/smoke`):

| kind | Verilator prints | process exit | cocotb sees |
|---|---|---|---|
| `$fatal` | `%Fatal: fatal.sv:31: ...`, then `Verilog $stop` | 1 | sim ends at 15 ns, test fails |
| `$stop` | `%Error: ...fatal.sv:32: Verilog $stop` | 1 | same |
| `$error` | `%Error: fatal.sv:33: ...`, then `Verilog $stop` (default error limit is 1) | 1 | same |
| `$finish` | `fatal.sv:34: Verilog $finish` | 0 | sim ends at **20 ns** (the next time step's timed callbacks still run), test fails with `SimFailure` |

- Top: `fatal`. Sources: `fatal.sv`.
