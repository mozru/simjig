// A covergroup next to ordinary line coverage (cpp/tests/main.cpp). Verilator registers the
// covergroup's bins when `new` runs (in the model's first eval, not its constructor), in
// counters it allocates on the heap, outside the symbol table: cov_runs leaves them out, and
// cov_write and cov_zero include them.
module tc_cg (
    input logic       clk,
    input logic [1:0] a,
    output logic      y
);
  covergroup cg @(posedge clk);
    coverpoint a;
  endgroup
  cg inst = new;
  always_ff @(posedge clk) begin
    if (a == 2'd3) y <= 1'b1;
    else y <= 1'b0;
  end
endmodule
