// Spike model for --timing: the fatal is raised inside a coroutine resumed by eval().
module delay(input logic go, output logic [7:0] stage);
  initial begin
    stage = 0;
    #10 stage = 1;
    $display("stage 1 at %0t", $time);
    #10 stage = 2;
    if (go) $fatal(1, "fatal in coroutine at %0t", $time);
    $display("after fatal");
    #10 stage = 3;
  end
endmodule
