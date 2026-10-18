// Spike-only design for experiment 5: an internal derived clock that pulses and clears
// inside ONE eval. `pulse` is set by posedge clk and asynchronously cleared by itself
// (through `clr`), so it is 1 only between delta iterations of Verilator's eval loop.
// `pulses` counts its rising edges inside the design. A testbench that samples `pulse`
// after eval() never sees it high.
module glitch (
    input  logic       clk,
    output logic [7:0] pulses
);
  logic pulse;  // starts at 0: Verilator zero-initialises 2-state storage (randReset 0)
  wire  clr = pulse;

  /* verilator lint_off SYNCASYNCNET */
  always_ff @(posedge clk or posedge clr) begin
    if (clr) pulse <= 1'b0;
    else pulse <= 1'b1;
  end
  /* verilator lint_on SYNCASYNCNET */

  always_ff @(posedge pulse) pulses <= pulses + 8'd1;
endmodule
