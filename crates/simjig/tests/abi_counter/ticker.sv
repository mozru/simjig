// Test model (--timing) for tests/abi_counter.rs: the $fatal runs inside a coroutine.
module ticker (
    input  logic       arm,
    output logic [7:0] ticks
);
  initial begin
    ticks = 0;
    forever begin
      #5 ticks = ticks + 8'd1;
      if (arm && ticks == 8'd3) $fatal(1, "ticker fatal at tick %0d", ticks);
    end
  end
endmodule
