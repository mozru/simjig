// Codegen test design for probe path checks: a three-level hierarchy with a parameter and
// a localparam, so that each kind of probe mistake has something to be wrong about.
module alu #(parameter int P = 3) (
    input  logic       clk,
    output logic [7:0] y
);
  localparam logic [7:0] LP = 8'h5a;
  logic [7:0] acc;
  mid u_mid (.clk(clk), .y(y));
  always_ff @(posedge clk) acc <= acc + LP + 8'(P);
endmodule

module mid (
    input  logic       clk,
    output logic [7:0] y
);
  alu_core u_core (.clk(clk), .y(y));
endmodule

module alu_core (
    input  logic       clk,
    output logic [7:0] y
);
  logic [7:0] result;
  always_ff @(posedge clk) result <= result + 8'd1;
  assign y = result;
endmodule
