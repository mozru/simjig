// Needs `--timing`. Generates its own clock with a `#` delay loop and times a pulse with
// an intra-procedural delay, so the next event time comes from the design
// (`nextTimeSlot()`), not from the testbench.
//   iclk:  internal clock, period 10 ns, first rising edge at 5 ns.
//   ticks: counts iclk rising edges since time 0.
//   done:  goes low on every rising edge of `start`, high again 23 ns later.
module timing_dut (
    input  logic       start,
    output logic       iclk_out,
    output logic [7:0] ticks,
    output logic       done
);
  timeunit 1ns; timeprecision 1ps;

  logic iclk = 1'b0;
  initial forever #5 iclk = ~iclk;

  initial ticks = '0;
  always @(posedge iclk) ticks <= ticks + 8'd1;

  initial done = 1'b1;
  always @(posedge start) begin
    done <= 1'b0;
    #23 done <= 1'b1;
  end

  assign iclk_out = iclk;
endmodule
