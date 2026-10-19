// A `public_flat_rw` variable made so by a metacomment, not by a probe: it is in no signal
// table row, only in the model's scope table. Verilator schedules the $time-reading logic
// it feeds in the 'ico' region under the 'first iteration' trigger, so every eval() re-runs
// it, written or not.
module rw_meta (
    input  logic        clk,
    output logic [63:0] t_out
);
  logic [7:0] knob  /*verilator public_flat_rw*/;
  initial knob = 8'd1;
  always_comb t_out = (knob != 8'd0) ? $time : 64'd0;
endmodule
