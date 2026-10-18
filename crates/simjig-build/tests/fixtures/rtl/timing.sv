// Needs `--timing` (fork, a delay, an event wait), and its port width depends on
// `VERILATOR_TIMING`, which Verilator defines only under `--timing`.
module timing (
  input logic clk,
`ifdef VERILATOR_TIMING
  output logic [7:0] q
`else
  output logic [3:0] q
`endif
);
  event e;
  initial begin
    fork begin #1; -> e; end join_none
    @(e);
  end
  always_ff @(posedge clk) q <= q + 1;
endmodule
