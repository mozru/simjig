// Types shared by struct_pipe and its testbenches.
package struct_pipe_pkg;
  localparam int PAYLOAD_W = 12;

  typedef enum logic [1:0] {
    OP_NOP = 2'd0,
    OP_ADD = 2'd1,
    OP_XOR = 2'd2,
    OP_INV = 2'd3
  } op_e;

  // Packed, MSB first: {op[1:0], tag[3:0], payload[11:0]} = 18 bits.
  typedef struct packed {
    op_e                  op;
    logic [3:0]           tag;
    logic [PAYLOAD_W-1:0] payload;
  } beat_t;
endpackage
