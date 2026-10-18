// Codegen test design: structs wider than 64 and 128 bits with fields that cross 32-bit
// word boundaries, a union over a struct, Rust keywords as names, and internal signals
// that the test probes (public_flat_rd and public_flat_rw).
package wide_pkg;
  localparam int unsigned PAD_W = 93;

  typedef enum logic [2:0] {
    MODE_OFF = 3'd0,
    MODE_RUN = 3'd5,
    MODE_ERR = 3'd7
  } mode_e;

  // 33 + 3 + 70 + 1 = 107 bits.
  typedef struct packed {
    logic [32:0] lo33;
    mode_e       mode;
    logic [69:0] mid70;
    logic        self;
  } part_t;

  // 107 + 93 + 107 = 307 bits.
  typedef struct packed {
    part_t            a;
    logic [PAD_W-1:0] loop;
    part_t            b;
  } big_t;

  typedef union packed {
    logic [106:0] raw;
    part_t        part;
  } part_u;

  typedef struct packed {
    logic [3:0] hi;
    logic [3:0] lo;
  } nib_t;
endpackage

module acc (
    input  logic       clk,
    input  logic [7:0] add,
    output logic [7:0] total
);
  logic [7:0] sum;
  always_ff @(posedge clk) sum <= sum + add;
  assign total = sum;
endmodule

module wide_top
  import wide_pkg::*;
(
    input  logic  clk,
    input  big_t  in_big,
    input  part_u in_u,
    input  nib_t  match,
    output big_t  out_big,
    output nib_t  out_nib,
    output mode_e out_mode,
    output logic [7:0] total
);
  mode_e state;
  logic [7:0] knob;

  assign out_big  = in_big;
  assign out_nib  = '{hi: match.lo, lo: match.hi};
  assign out_mode = state;
  always_ff @(posedge clk) state <= in_u.part.mode;

  acc u_acc (
      .clk(clk),
      .add(knob),
      .total(total)
  );
endmodule
