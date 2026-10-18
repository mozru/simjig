// A D flip-flop with a synchronous, active-high reset.
module flop (
    input  logic clk,
    input  logic rst,
    input  logic d,
    output logic q
);
  timeunit 1ns; timeprecision 1ps;

  always_ff @(posedge clk) begin
    if (rst) q <= 1'b0;
    else q <= d;
  end
endmodule
