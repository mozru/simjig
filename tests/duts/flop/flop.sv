// D flip-flop with synchronous active-high reset. `q` changes only on posedge clk.
module flop (
    input  logic clk,
    input  logic rst,
    input  logic d,
    output logic q
);
  always_ff @(posedge clk) begin
    if (rst) q <= 1'b0;
    else q <= d;
  end
endmodule
