// Ports at every width class Verilator stores differently: CData (1, 8), IData (<=32),
// QData (33..64) and VlWide (>64). Each output is its input inverted, combinationally,
// plus a registered copy of the 200-bit input so wide flops are covered too.
module wide (
    input  logic         clk,
    input  logic         i1,
    input  logic [  7:0] i8,
    input  logic [ 32:0] i33,
    input  logic [ 63:0] i64,
    input  logic [ 64:0] i65,
    input  logic [199:0] i200,
    output logic         o1,
    output logic [  7:0] o8,
    output logic [ 32:0] o33,
    output logic [ 63:0] o64,
    output logic [ 64:0] o65,
    output logic [199:0] o200,
    output logic [199:0] q200
);
  assign o1   = ~i1;
  assign o8   = ~i8;
  assign o33  = ~i33;
  assign o64  = ~i64;
  assign o65  = ~i65;
  assign o200 = ~i200;

  always_ff @(posedge clk) q200 <= i200;
endmodule
