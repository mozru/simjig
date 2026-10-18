// A deliberately different revision of tests/duts/counter/counter.sv, for lockstep tests:
// the same module name and ports, but the count skips 6 (5 -> 7). Built under its own
// --prefix, so both revisions link into one test binary.
module counter #(
    parameter int WIDTH = 8
) (
    input  logic             clk,
    input  logic             rst,
    input  logic             en,
    output logic [WIDTH-1:0] count
);
  always_ff @(posedge clk) begin
    if (rst) count <= '0;
    else if (en) count <= (count == WIDTH'(5)) ? WIDTH'(7) : count + 1'b1;
  end
endmodule
