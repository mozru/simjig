// Signed ports and the integer atom types (IEEE 1800-2023 §6.11).
module top (
  input  logic signed [15:0] s16,
  input  logic        [15:0] u16,
  input  bit   signed [4:0]  s5_2state,
  input  byte                by,
  input  shortint            si,
  input  int                 i,
  input  longint             li,
  input  integer             ig,
  input  time                tm,
  input  logic signed [79:0] s80,
  output logic signed [15:0] neg
);
  assign neg = -s16;
endmodule
