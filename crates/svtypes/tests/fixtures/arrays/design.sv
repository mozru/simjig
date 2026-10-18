// Packed arrays (multi-dimensional, of structs, signed) and unpacked-array ports.
package pkg;
  typedef struct packed { logic [2:0] a; logic b; } pair_t;
endpackage

module top (
  input  logic [3:0][2:0]        pa2,
  input  logic [1:4][0:2]        pa_asc,
  input  pkg::pair_t [2:0]       pa_struct,
  input  logic signed [1:0][3:0] pa_signed,
  input  logic [7:0]             ua [4],
  input  logic [7:0]             ua2 [3:0][1:0],
  input  logic [99:0]            ua_wide [2],
  output logic [7:0]             ua_out [3:1]
);
  assign ua_out[1] = ua[0];
  assign ua_out[2] = ua2[0][0];
  assign ua_out[3] = pa2[0] ^ pa_asc[1] ^ pa_struct[0] ^ pa_signed[0] ^ ua_wide[0][7:0];
endmodule
