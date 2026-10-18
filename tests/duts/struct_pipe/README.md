# struct_pipe

Two-stage pipeline over a packed struct from a package.

`struct_pipe_pkg` defines:
- `localparam int PAYLOAD_W = 12`;
- `typedef enum logic [1:0] { OP_NOP, OP_ADD, OP_XOR, OP_INV } op_e`;
- `typedef struct packed { op_e op; logic [3:0] tag; logic [PAYLOAD_W-1:0] payload; } beat_t`,
  18 bits, MSB first: `{op[17:16], tag[15:12], payload[11:0]}`.

Stage 1 registers `in_beat`; stage 2 registers `op(payload, operand)` (ADD: `+`, XOR: `^`,
INV: `~payload`, NOP: unchanged), with `tag` and `op` passed through. Latency: 2 cycles, no
back-pressure. `operand` is sampled at the second edge.

- Top: `struct_pipe`. Sources, in order: `struct_pipe_pkg.sv`, `struct_pipe.sv`.
- Smoke: `tests/duts/smoke` (`struct_pipe`).
