// One port per header storage class and direction, for the header-parser test.
module ports (
  input  logic          b1,
  input  logic [7:0]    c8,
  input  logic [8:0]    s9,
  output logic [15:0]   s16,
  input  logic [16:0]   i17,
  output logic [31:0]   i32,
  input  logic [32:0]   q33,
  output logic [63:0]   q64,
  input  logic [64:0]   w65,
  output logic [127:0]  w128,
  output logic [5:2]    off,
  inout  wire  [3:0]    bidir
);
  assign s16 = {s9[7:0], c8} ^ {15'b0, b1};
  assign i32 = {15'b0, i17};
  assign q64 = {31'b0, q33};
  assign w128 = {63'b0, w65};
  assign off = c8[3:0];
endmodule
