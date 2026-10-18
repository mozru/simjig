// A --timing design with its own delays, traced: checks that kTiming and kTraceFst combine.
module tc_timing (
    output logic [7:0] ticks
);
  initial begin
    ticks = 8'd0;
    repeat (6) begin
      #10 ticks = ticks + 8'd1;
    end
  end
endmodule
