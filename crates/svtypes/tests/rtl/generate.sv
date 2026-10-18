// Generate blocks in the IR hierarchy (tests/it/runner.rs).
module gen_top #(parameter bit EN = 1) (input logic clk);
  if (EN) begin : g_x
    gen_cell u_x (.clk(clk));
    if (EN) begin : inner
      gen_cell u_in (.clk(clk));
    end
  end
  if (EN) begin
    gen_cell u_anon (.clk(clk));
  end
  for (genvar i = 0; i < 2; i++) begin : g
    gen_cell u (.clk(clk));
    logic t;
  end
  gen_cell arr[1:0] (.clk(clk));
endmodule

module gen_cell (input logic clk);
  logic s;
  always_ff @(posedge clk) s <= ~s;
endmodule
