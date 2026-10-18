// Coverage points in a module Verilator keeps as its own object (no_inline_module), so they
// are not in the top's counter array: cov_runs must still return them, one run per object.
module tc_hier (
    input  logic clk,
    input  logic a,
    output logic y0,
    output logic y1
);
  tc_leaf l0 (.clk(clk), .a(a), .y(y0));
  tc_leaf l1 (.clk(clk), .a(~a), .y(y1));
endmodule

module tc_leaf (
    input  logic clk,
    input  logic a,
    output logic y
);
  /*verilator no_inline_module*/
  always_ff @(posedge clk) begin
    if (a) y <= 1'b1;
    else y <= 1'b0;
  end
endmodule
