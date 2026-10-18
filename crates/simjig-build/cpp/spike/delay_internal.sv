// Spike model for --timing: an *internal* Verilator fatal ($readmem of a missing-syntax file)
// raised inside a coroutine.
module delay_internal(output logic [7:0] stage);
  logic [7:0] mem [0:3];
  initial begin
    stage = 0;
    #10 stage = 1;
    $readmemh("bad.hex", mem);
    stage = 2;
  end
endmodule
