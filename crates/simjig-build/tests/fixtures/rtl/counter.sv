// Free-running counter with synchronous active-low reset. The increment comes from an
// included file, so the tests can check that editing an include invalidates the cache.
`include "defs.svh"
module counter #(parameter int WIDTH = 8) (
  input  logic             clk,
  input  logic             rst_n,
  output logic [WIDTH-1:0] count
);
  always_ff @(posedge clk)
    if (!rst_n) count <= '0;
    else        count <= count + WIDTH'(`STEP);
endmodule
