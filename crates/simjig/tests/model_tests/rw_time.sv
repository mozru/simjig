// A public_flat_rw variable (`knob`, probed read-write by build.rs) feeding combinational
// logic that reads $time: Verilator schedules it in the 'ico' region under the 'first
// iteration' trigger, so every eval() re-runs it, written or not.
module rw_time (
    input  logic        clk,
    output logic [63:0] t_out
);
  logic [7:0] knob;
  initial knob = 8'd0;
  always_comb t_out = (knob != 8'd0) ? $time : 64'd0;
endmodule
