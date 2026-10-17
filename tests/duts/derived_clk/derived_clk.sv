// An internal clock divided by two from `clk`. `div_clk` toggles on every posedge clk, so
// it rises on every second posedge clk, in the same eval, after the clk-domain flops.
// `slow_count` counts div_clk rising edges; `fast_count` counts clk rising edges.
// Testbench triggers on div_clk are on a derived clock and are therefore sampled
// (DECISION.md §1).
module derived_clk (
    input  logic       clk,
    input  logic       rst,
    output logic       div_clk,
    output logic [7:0] fast_count,
    output logic [7:0] slow_count
);
  always_ff @(posedge clk) begin
    if (rst) begin
      div_clk    <= 1'b0;
      fast_count <= '0;
    end else begin
      div_clk    <= ~div_clk;
      fast_count <= fast_count + 8'd1;
    end
  end

  // div_clk is held low during reset, so the slow domain cannot see a synchronous reset;
  // it takes rst asynchronously on purpose.
  /* verilator lint_off SYNCASYNCNET */
  always_ff @(posedge div_clk or posedge rst) begin
    if (rst) slow_count <= '0;
    else slow_count <= slow_count + 8'd1;
  end
  /* verilator lint_on SYNCASYNCNET */
endmodule
