// Prints on every rising edge of `clk` and warns once, so the tests can check that model
// output reaches the test's own output. `n` counts the edges.
module hello (
    input  logic       clk,
    output logic [7:0] n
);
  initial n = 8'd0;
  always @(posedge clk) begin
    $display("hello: edge %0d in %m", n);
    if (n == 8'd1) $warning("hello: second edge");
    n <= n + 8'd1;
  end
endmodule
