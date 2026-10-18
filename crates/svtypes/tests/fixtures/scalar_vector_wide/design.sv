// Scalar, vector and wide ports: one port on each side of every C++ storage threshold
// (CData <= 8, SData <= 16, IData <= 32, QData <= 64, VlWide above), plus names the
// model generator must mangle for C++.
module top (
  input  logic        clk,
  input  logic [7:0]  b8,
  input  logic [8:0]  b9,
  input  logic [15:0] b16,
  input  logic [16:0] b17,
  input  logic [31:0] b32,
  input  logic [32:0] b33,
  input  logic [63:0] b64,
  input  logic [64:0] b65,
  input  logic [95:0] b96,
  output logic [99:0] wide,
  input  bit   [3:0]  two_state,
  input  logic [0:7]  ascending,
  input  logic [3:-2] negative_lsb,
  inout  wire  [3:0]  bidir,
  input  logic        \esc$name ,
  input  logic        char
);
  assign wide = {b96, b8[3:0]} ^ {99'b0, char ^ \esc$name ^ clk};
endmodule
