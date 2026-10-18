// Spike-only design: no shared DUT uses $urandom or $display, so the multi-model spike
// needs this one. On every posedge clk it draws one $urandom value into `rnd` and
// prints `$time` and the value with $display, so a testbench can check which context's
// time, random stream and output channel the model used.
module rng_disp (
    input  logic        clk,
    output logic [31:0] rnd
);
  always_ff @(posedge clk) begin
    rnd <= $urandom;
    $display("rng_disp t=%0t rnd=%08x", $time, rnd);
  end
endmodule
