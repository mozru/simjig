// Uses a covergroup, so its model needs Verilator's covergroup runtime, whose code
// depends on `--coverage`.
module cg_user (
  input logic       clk,
  input logic [1:0] a
);
  covergroup cg @(posedge clk);
    coverpoint a;
  endgroup
  cg inst = new;
endmodule
