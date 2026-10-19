// A design that writes its own input (its ASSIGNIN lint waived), feeding combinational
// logic that reads $time: Verilator schedules that logic under the 'ico' region's 'first
// iteration' trigger, so every eval() re-runs it, written or not. Nothing in the build
// shows the write, so simjig cannot turn eval elision off for it by itself.
module input_time (
    input  logic        clk,
    input  logic [7:0]  knob,
    output logic [63:0] t_out
);
  /* verilator lint_off ASSIGNIN */
  initial knob = 8'd1;
  /* verilator lint_on ASSIGNIN */
  always_comb t_out = (knob != 8'd0) ? $time : 64'd0;
endmodule
